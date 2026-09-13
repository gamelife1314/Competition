# 对战数据交付规范（workflow agent 接口）

> **版本 v11** · 本文档是内网自动对战 workflow 的**交付规范**。
> 读者：拉代码 → 发起对战 → 抓日志 → 分析对局 → 生成改进 issue 的 code agent。
> **v11 改的是交付通道**：没有文件路径可用，我拿不到你机器上的任何东西——**issue 正文
> 就是交付物**。§1 已按这条重写，§7.3 因此给每张表加了**行数预算**（按真实批次量过），
> §8 的 `deliverables` 也从 `path:` 改成 `paste:`。对手日志与"原始行 tsv"这两项不再要，
> 理由见 §1。
> 其余与 v10 相同：v10 把表 5（封门）加厚了——`wall_gate_open` 现在带 `away`/`stuck`，
> 能直接读出**门卡在谁身上**，并且明确了"这个窗口总共只有 15 回合，15 行 = 整夜没封"。
> §7 是正面清单（issue 正文要给我哪五部分、怎么取、长什么样，全部给示例），你不需要回答
> 我任何问题，把六张表填出来交给我是唯一的要求。§6 是你可以自己跑一遍的自检清单；
> §8 是同一份内容的机器可读版本。

---

## 0. 一句话

每批在 issue 正文里给：头部 8 个字段 + **每场一份结果回执** + **六张证据表**（§7）。
**没有文件路径这回事**——stdout 之外的东西我拿不到，贴进 issue 的字就是全部（§1）。

分界线只有一条：**棋盘我看得见，结果我看不见。**

每个回合的请求里带着：回合号、地图、我方队名/ID/金币/总分、我方任务板、**双方所有
单位的血量与等级**、机器人、两家商店、判题器的错误码与原文。但 `teamEnemy` 里**只有
`roles`**——对方的总分、金币、任务提交，以及**这一场谁赢了**，我一律看不到。

所以这件事分成两半，各归各管：

| 半边 | 谁提供 | 怎么到我手里 |
|---|---|---|
| 逐回合过程数据 | 我（stdout JSONL） | 你留着**没加工过的那份**，按 §7.3 的配方跑，把**输出**贴进 issue |
| 结果、身份、版本 | 你 | 按 §3 的字段填进回执，贴进 issue |

---

## 1. 交付物清单

**交付通道只有一条：issue 正文。** 你机器上的任何路径我都拿不到——文件下载到本地之后
就停在本地了，我读到的只有贴进 issue 的字。所以本规范的"交付"= **把下面的东西贴进 issue**，
不是"把文件放在某个目录里"。

| # | 内容 | 从哪来 | 硬性要求 |
|---|---|---|---|
| 1 | §4 的 8 个头部字段 | 平台页面 + §5 教练配方 | 每批一次 |
| 2 | **每场一份** `receipt.json`（§3），整段贴 | 对局详情页 | 字段一个不少，取不到填 `null`；一个 `match_id` 一份，不合并 |
| 3 | §5 的教练汇总一行 | 你的 `ours.jsonl` | 格式固定，我会 grep |
| 4 | **§7.3 的六张表**，每张贴配方的**输出原文** | 你的 `ours.jsonl` | 见 §7.3 的行数预算 |
| 5 | §7.4 缺口：拿不到的东西和原因 | 你自己 | 不许静默省略 |

`workflow/` 那套目录（`workflow/logs/<match_id>/…`）**是给你自己用的**：原始 stdout 存在
哪里、叫什么名字、怎么组织，与本规范无关，也不必在 issue 里报路径。**只要保证随时能重跑
配方**——表里的每一行都是从原始 stdout 跑出来的，我问"某天黄昏门卡在谁身上"的时候，你要能
当场再跑一条贴上来。

**不再要的两项**（v10 及更早的清单里有，现在取消）：

- **对手日志 `enemy.jsonl`**。它到不了我手里，而我要的那点对手事实（基地等级 / 塔数 / 墙数）
  已经在 `receipt.enemy_seen` 里了。拿不到也不必写 `.missing`，§7.4 提一句就够。
- **"原始行另存 tsv + 给路径"**。路径对我无效。原始行留在本地备查，贴的是**配方的输出**。

每批：issue 头部 8 个字段（§4）+ 每场一份 `receipt.json`（§3）+ 一行教练汇总（§5）+
**§7.3 的六张证据表**（整个 issue 的正文就是 §7 那五部分）。

---

## 2. 我方日志：本地那份要整份留着，别加工

> 这一节讲的是**你本地保存的那份原始 stdout**。它不进 issue（太大），但 §7.3 的每条配方都
> 跑在它上面——所以它一旦被截断、被合并、被"顺手清理"，表就跟着失真，而我无从察觉。
> 贴进 issue 的是配方的输出，不是这个文件。

**格式**：stdout 是 JSONL，一行一个事件：

```json
{"event":"round","data":{"round":1,"day":1,…}}
```

- **首行不是 JSON**（`listening on 0.0.0.0:<port>`）。解析前跳过不以 `{` 开头的行。
- 带 `round` 字段的记录**没有 `ts`**：行序就是时间线，`round` 自身就是时钟。只有
  `startup` / `build_info` / `coach_*` 这些没有回合可锚的记录才带 `ts`。
- 每回合一条 `round`，另有一批 `task_*` / `coach_*` / `volley_review` / `night_debug` /
  `shopping` / `wall_build` … 事件混在同一流里。**回合数 ≠ 行数**，别按行数截取回合。
- **行序有意义**（事件按发生顺序落盘）。重排、按事件名分组都会破坏因果链。
- 本地怎么存都行（gzip、按场分目录、随便命名），**前提是重跑配方时能还原成原始行序**。

**三个必须知道的压缩规则**（不知道就会把"没写"读成"没发生"）：

1. **空值不落盘。** `null`、`""`、`[]`、`{}` 一律不写 —— 缺键和空值同义。但
   **`0` 和 `false` 是事实，照写**：`gold:0` 是没钱，`noRobotDamage:false` 是对本回合的
   断言。所以 `jq 'select(.data.errors)'` 能筛出"有错"的回合，而
   `jq 'select(.data.gold==0)'` 也照常工作。
2. **没变的块不重写。** `round` 里的 `stationHp`/`enemyStationHp`/`wall`/`enemyWall`/
   `towers`/`roles`/`pairs`/`task`/`treasure` **只在变化的那一回合写**，其余回合整个键
   缺席 —— **缺键 = 沿用上一次出现的值，不是"没有"**。那一回合重写了哪些块，由
   `round.data.chg` 列出；**没有 `chg` 就是本回合无变化**。
   **`chg` 是权威，键不是**：块名在 `chg` 里、键却不在记录里，意思是这个块**变成了空的**
   （塔全被拆光、配对清空、任务会话结束）—— 空值本身不落盘（规则 1），所以由 `chg` 宣布。
   只看键的读者会在这一回合继续沿用旧值，错得无声无息。要取序列必须先"带值前行"：
   ```sh
   grep '^{' ours.jsonl | jq -c 'select(.event=="round")
     | {round:.data.round, towers:(.data.towers // "same"), wall:(.data.wall // "same")}'
   ```
   逐回合必写的只有：`round`/`day`/`isDay`/`gold`/`score`/`scoreDelta`/`scoreAttr`/
   `robotCount`/`cmds`/`policy`/`volley`/`ms`，以及非空时的
   `errors`/`errorDescs`/`failures`/`robotEvents`/`phaseTask`/`lastCmdResult`。
   基地陷落写 `stationHp: 0`（不是缺键）；对手基地写 `null` 表示**看不见**，不等于被拆。
3. **按对/按塔的流已合并。** `night_debug` 现在是**每回合一条**，内含 `pairs[]`，每项
   `{tower, controller, reason, fired?, hp?}`；`reason` 就是全部结论
   （`controller_walking`/`controller_stuck` 是夜召回的两种结局，`cooldown` 是炮在冷却，
   `no_target_in_range` 与 `no_target_reserved_for_robots` 区分"没目标"和"我们自己留着不打"）。
   它**每回合都写**，因为"某塔 `controller_withdrawn` 了多少回合"本身就是结论 ——
   计数行数即可，别去重。`night_recall` 与 `night_withdraw` 已并入 `night_debug`。

**禁止**（对自己本地那份）：截断长字段、合并多行、给每行加队伍前缀或时间戳注解、脱敏
ID/坐标、只保留 `round` 事件、只保留你分析里引用到的那几行。这些都让配方在你手上和在我
手上跑出不同的结果，而我看不出来。

我已经**自己**截断过的字段（不必再想办法还原，但分析时别把它们当全文）：

| 字段 | 上限 | 出现在 |
|---|---|---|
| `errorDescs[]` | 120 字符 | `round` |
| `phaseTask`、`lastCmdResult` | 160 字符 | `round` |
| `task_ended.bestAnswer` | 120 字符 | `task_ended` |
| `task_answer_found.answer` / `task_answer_sentinel.answer` / `task_answer_blocked.answer` | 120 / 60 / 60 字符 | 各自事件 |
| `prompt_sent.head`、`cmd_sent.head` | 300 字符 | 各自事件 |
| `news_official.head`、`news_legend.head` | 200 字符 | 各自事件 |

> 被截断的字符串末尾带 `…`——**看到省略号就是它被裁过**，别把后半段当"原文里没有"。

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
7. 对手那一侧拿不到的东西，在 §7.4 写明原因，而不是静默省略。
8. §7.3 的六张表**每张都跑过**（哪怕命中 0 行也要有那张表和一个 0），且每张表都不是
   一句结论。
9. issue 正文里**没有**任何"请你回答"式的反向提问——不存在这样的问题。
10. 表 5 命中 15 行的那天，写的是"**整夜没封**"而不是"开了 15 回"；`away`/`stuck` 里出现的
   角色 id 在表里点名了——门卡在谁身上是这张表存在的理由。
11. 六张表的输出**加起来 ≤ 400 行**（贴之前 `… | wc -l` 加起来看一眼）。超了就是贴了原始行
   而不是配方输出，回 §7.3 换成那张表的汇总式——**表 5 例外**，它开得久正是要看的东西。
12. **issue 正文里不出现任何本地路径**。`workflow/logs/…` 这种写给自己看就行：路径到我这里
   是死字，我打不开。

---

## 7. issue 正文：五部分

放进 issue 的信息就是这五部分。前两部分是你对平台数据的转写，第三部分是从 `ours.jsonl`
跑出来的**六张表**，第四部分是取不到的东西，第五部分是"别再报这个"。

**格式不必和我一样，字段和内容要在。** 每部分都给了取法和一段示例输出。
示例里的数字是编的，只为说明表的形状——不要把它当成任何一场的实际数据。

---

### 7.1 第一部分：批级概况

§4 的 8 个字段。来源：平台页面 + §5 的教练配方。

```text
生成时间：2026-09-14 09:12
对战代码版本：pk580001: 9f3c2a1 (2026-09-14 07:40)
              pk580002: 9f3c2a1 (2026-09-14 07:40)
本批次数 / 对手数：6 场 / 3 个对手
当前排名：9 / 64
累计战绩：14 胜 9 负 2 平
胜率：56.0%
每场一行结果：
  pk580001 胜 139:52 (9f3c2a1)
  pk580002 负  41:88 (9f3c2a1)
  pk580003 平  60:60 (9f3c2a1)
  ...
教练：本批 6 场 | 移动 4 次（station_pressure 2 / gap_funding 1 / harass 1） | 至少移动过一次的场次 3/6 | 结果分布 胜 4 / 负 1 / 平 1
```

---

### 7.2 第二部分：逐场结果回执

一场一份 `receipt.json`，字段表见 §3.3，示例见 §3.2。**取不到就填 `null`，不要用 `0` 冒充**
（§3.4 规则 1）。这一部分 §3 已经写全，这里不再重复。

---

### 7.3 第三部分：六张证据表

**这是整份 issue 里我最需要的部分**，也是唯一能从日志里挖出来的部分——胜负你能看见，过程
只有我能看见，而这六张表就是过程的切片。

统一约定：

- 每条配方都以 `grep '^{' ours.jsonl |` 开头，跳过首行非 JSON 的 `listening on` 行（§2）。
  `ours.jsonl` 是你本地那份原始 stdout，路径你自己知道就行——**贴的是配方的输出**。
- **每张表都有行数预算**。下表的"实测"是拿一场 10 天 / 3 座塔 / 16 个 session 的对局按真实
  批次的形状跑出来的：**六张表全部跑完 193 行 / 4.8 KB**，平均每张 32 行。某张表远超它那一行，
  只有两种可能——要么这场真的异常（**那就贴**，并在表下写一句哪里异常），要么你贴的是原始行
  而不是配方输出（回去换汇总式）。

| 表 | 配方 | 实测行数 | 预算 | 增长源 |
|---|---|---|---|---|
| 1 造塔计划 | 1 | 3 | ≤ 15 | 聚合式，恒定 |
| 2 经济台账 | 2a / 2b / 2c | 33 / 2 / 50 | ≤ 150 | 钱动了多少回 |
| 3 任务结局 | 3a / 3b | 2 / 15 | ≤ 40 | session 数 |
| 4 判题器裁定 | 4a / 4b | 14 / 8 | ≤ 80 | 提交次数 / 出错回合数 |
| 5 封门 | 5a / 5b / 5c | 52 / 3 / 1 | ≤ 80（5a） | **每天最多 15 行** |
| 6 夜间塔况 | 6a / 6b | 3 / 7 | ≤ 40 | 聚合式，恒定 |

- 表 5 的 5a 是唯一按回合线性长的：黄昏窗口一天就 15 回合，所以**每天封顶 15 行**，10 天最多
  150 行加 10 行封门。它超预算不是"贴多了"，是"门开太久了"——正是我要看的，**照贴**。
  真想省地方就先贴 5b（每天几行），我点名要哪天的 5a 再补。
- 总量目标 **≤ 400 行 / 15 KB**。issue 正文上限 65536 字符，这个预算留了 4 倍余量给回执和正文。
- 不要只贴头几行，也不要用"结论：正常"代替表格——结论由我来下，你给我行。
- 表下可以写一两句你怎么读它，但**行必须在**。

---

#### 表 1 · 造塔计划 `tower_plan`

**我用它判断**：第三座塔为什么没造出来。`mayBuild`（手里的钱够不够买武器）和
`upgradeReachable`（够不够升级基地）是两个不同的门，如果它们轮流为真却谁也没落地，
卡点就不在钱上，而在"没人走到工地"。

```sh
grep '^{' ours.jsonl | jq -r 'select(.event=="tower_plan")
  | "\(.data.towers)\t\(.data.mayBuild)\t\(.data.upgradeReachable)\t\(.data.reserve)\t\(.data.guard)"' \
  | sort | uniq -c
```

示例输出（第一列是回合数，后面依次是 塔数 / mayBuild / upgradeReachable / reserve / guard）：

```text
     18 2	true	false	25	false
     34 2	false	true	25	false
      6 2	false	false	25	true
```

**逐回合的原始行不要贴进 issue**——它是 700 行，预算是 15。留在本地，或者我点名要哪一天时
再跑（下面按"第 N 天"筛，`N` 自己换）：

```sh
grep '^{' ours.jsonl | jq -r 'select(.event=="tower_plan")
  | select((((.data.round-1)/130)|floor)+1 == 5)
  | [.data.round,.data.towers,.data.gaps,.data.reserve,.data.guard,.data.wallGaps,.data.stoneDemand,.data.teamStone,.data.mayBuild,.data.upgradeReachable] | @tsv'
```

---

#### 表 2 · 经济台账 `buy` / `sell` / `shopping`

**我用它判断**：金币冻结是**收入端**没进账，还是**支出端**被守卫金挡住。三个事件要一起看：
`sell` 是进账，`buy` 是出账，`shopping` 是"想买但买没买到"（`affordable` 为 false 的回合占比）。

```sh
# 2a 台账：一进一出各一行（事件名说明方向，两个方向字段完全一样）
grep '^{' ours.jsonl | jq -r 'select(.event=="buy" or .event=="sell")
  | [.data.round,.event,.data.role,.data.item,.data.num,.data.gold] | @tsv'
```

示例输出：

```text
5	sell	10002	iron	4	25
9	buy	10002	WallFixer	1	0
23	sell	10003	iron	4	25
31	buy	10002	WallFixer	1	0
```

```sh
# 2b 想买 vs 买得起：affordable=false 的回合数，按 head 需求分组
grep '^{' ours.jsonl | jq -r 'select(.event=="shopping") | "\(.data.affordable)\t\(.data.need)"' \
  | sort | uniq -c
```

示例输出：

```text
    612 false	WeaponUpgradeVoucher1
    140 true	WallFixer
     44 false	WeaponUpgradeVoucher1
```

```sh
# 2c 钱包台阶：只留金币真正变动的那几回合（`round.gold` 是每回合必写的，整场贴太长）
grep '^{' ours.jsonl | jq -r 'select(.event=="round") | "\(.data.round)\t\(.data.gold)"' \
  | awk -F'\t' '$2 != prev {print; prev = $2}'
```

示例输出（回合 / 金币——一眼看出是不是长期贴死在 25）：

```text
1	75
9	0
23	25
160	100
```

> 2c 是备选：2a/2b 已经说明问题就不必给。

**`shopping` 的逐回合原始行不要贴进 issue**（一场几百行，2b 已经把它压成两行）。本地备查，
或者我点名要哪一段时再跑：

```sh
grep '^{' ours.jsonl | jq -r 'select(.event=="shopping")
  | select((((.data.round-1)/130)|floor)+1 == 5)
  | [.data.round,.data.affordable,.data.need,.data.needNum,.data.price,.data.gold,.data.reserve,.data.buyerShopDist,.data.deadline] | @tsv'
```

---

#### 表 3 · 任务结局

**我用它判断**：12 个 session 全 0 分，"超时"和"判错三次"是两种完全不同的病——
超时结束时判题器按此前提交过的最好答案结算（任务书 `timeoutRounds` 条），所以
`reason=timeout` **不等于** 0 分。

```sh
# 3a 结局直方图
grep '^{' ours.jsonl | jq -r 'select(.event=="task_ended") | "\(.data.reason)\t\(.data.success)"' \
  | sort | uniq -c
```

示例输出：

```text
      7 timeout	true
      3 timeout	false
      2 wrong_answers	false
```

```sh
# 3b 逐 session 一行（和 receipt.tasks_ours[] 对得上）
grep '^{' ours.jsonl | jq -r 'select(.event=="task_ended")
  | [.data.session,.data.reason,.data.success,.data.wrongAnswers,.data.cmdRounds] | @tsv'
```

示例输出：

```text
1	timeout	true	1	37
2	timeout	true	0	22
3	wrong_answers	false	3	58
```

---

#### 表 4 · 判题器裁定

**我用它判断**：0 分**唯一的现场证据**。

判题器的 `errorCode` 是**粗分类**，官方就这六个（《接口文档》Response 一节）：

| `errorCode` | 含义 |
|---|---|
| 0 | 未知错误 |
| 1 | 任务超时 |
| 2 | **答案错误**（提交的答案不正确**或不完全正确**） |
| 3 | 网络错误 |
| 4 | 指令错误 |
| 5 | LLM 额度超限 |

> **别把 `2` 当成"值算错了"**：字段名错、字段缺、值超范围、数值不对，全落在 `2` 里。
> 真正能区分的是 `description`——判题器的**原话**（如 `MissingNamedInput: city` 指名道姓
> 说缺 `city`）。所以 4b 的两列要**一起**看：`errors` 是码，`errorDescs` 是原话，两个数组
> **同序**（第 i 个码对应第 i 句原话）。

```sh
# 4a 每次提交一行（chars 是提交原文的真实字符数，answer 全文见 task_answer_submit.answer）
grep '^{' ours.jsonl | jq -r 'select(.event=="task_answer_submit")
  | [.data.round,.data.session,.data.chars,.data.flipped,.data.rewritten,.data.wrongSoFar] | @tsv'
```

示例输出（回合 / session / 字符数 / 换过外形 / 被改写 / 当时已错几次）：

```text
48	1	212	false	false	0
61	1	208	true	false	1
77	2	96	false	false	0
```

```sh
# 4b 判题器回过错的回合：第2列是错误码，第3列是判题器原话（同序）
grep '^{' ours.jsonl | jq -r 'select(.event=="round") | select(.data.errors)
  | [.data.round, (.data.errors|join(",")), (.data.errorDescs // [] | join(" | "))] | @tsv'
```

示例输出：

```text
48	2	MissingNamedInput: city
61	2	MissingNamedInput: city
77	2	字段 humidity 的值不在合法区间
```

> 4b 是**全量**的错误回合，不限于任务——`cmds` 被拒、技能坐标非法也在这里，一并给我。
> 提交的那一回合与它之后几回合**都要**，别只截提交回合本身：判题器的回执常常晚一两回合到。

---

#### 表 5 · 封门

**我用它判断**：城门开了几回合，以及**卡在谁身上**。每天黄昏（`dayRound >= 55`）检查一次
"所有人归队了吗"，归队就封（`wall_gate_seal`），没归队就开（`wall_gate_open`）并**每回合重报**
直到封上——所以 `wall_gate_open` 的行数**就是**那天门开着没封上的回合数。这个数直接对应城墙能不能撑住。

**关键：这个窗口总共只有 15 回合**（`dayRound` 55..69，之后就是夜里了）。所以**一行都没封上
= 15 行**，意思是**整夜门都开着**，不是"开了 15 回"。看到 15 就要当成城墙不存在来读。

`away` 是没归队的角色（`[id, x, y]`）；`stuck` 是其中**根本走不回岗位**的那几个——被墙或机器人
隔开，永远到不了。"在路上"只是门晚封一两回合，"stuck"是整夜不封。两个字段空了会被裁掉。

```sh
grep '^{' ours.jsonl | jq -r 'select(.event=="wall_gate_seal" or .event=="wall_gate_open")
  | [.data.round, (((.data.round-1)/130)|floor)+1, ((.data.round-1)%130)+1, .event,
     ((.data.away // []) | tostring), ((.data.stuck // []) | tostring)] | @tsv'
```

示例输出（回合 / 第几天 / 当天第几回合 / 事件 / 没归队的 / 走不回去的）：

```text
185	2	56	wall_gate_open	[[10004,13,26]]	[]
186	2	57	wall_gate_open	[[10004,13,26]]	[]
187	2	58	wall_gate_open	[[10002,33,13]]	[10002]
188	2	59	wall_gate_seal	[]	[]
```

```sh
# 每天开了几回合（一行一天，最快看出哪天没封上；15 就是整夜没封）
grep '^{' ours.jsonl | jq -r 'select(.event=="wall_gate_open") | (((.data.round-1)/130)|floor)+1' \
  | sort -n | uniq -c
```

示例输出：

```text
      3 1
     15 2
      2 4
```

```sh
# 卡在谁身上：哪个角色、在哪、卡了几回合（整场累计）
grep '^{' ours.jsonl | jq -r 'select(.event=="wall_gate_open") | .data.away[]? | @tsv' \
  | awk -F'\t' '{c[$1]++; last[$1]=$2","$3} END {for (id in c) print id, c[id], last[id]}' | sort -k2 -nr
```

示例输出（角色 / 没归队的回合数 / 最后一次出现的位置）：

```text
10004	5	13,26
10002	2	33,13
```

---

#### 表 6 · 夜间塔况 `night_debug`

**我用它判断**：塔在夜里到底在干什么。**它每回合都写**，所以"某塔 `controller_withdrawn`
了多少回合"本身就是结论——**计数，不要去重**。

`reason` 的**全部** 8 个取值，别把第一个当成故障：

| `reason` | 意思 |
|---|---|
| `fired` | **默认值 = 塔开火了**，不是"闲置" |
| `controller_withdrawn` | 操作员带伤脱离，炮位没人（行内另带 `hp`） |
| `controller_walking` / `controller_stuck` | 夜召回的两种结局：走得回来 / 走不回来 |
| `controller_healing` | 操作员在吃药 |
| `cooldown` | 炮在冷却 |
| `no_target_in_range` | 射程内没有目标 |
| `no_target_reserved_for_robots` | 有机器人猎物，但被我们的开火纪律留着不打 |

> 两个坑：**(1) `reason` 缺省是 `fired`**，直接对 `reason` 做直方图会把"开火"混进"没开火"
> 的原因里——要看沉默原因，先 `select(.data.reason != "fired")`。**(2) `pairs` 为空时整个键
> 不落盘**（§2 规则 1），所以配方用 `[]?` 而不是 `[]`，否则 jq 报
> `Cannot iterate over null`。

```sh
# 6a 按原因计数（每场一行）
grep '^{' ours.jsonl | jq -r 'select(.event=="night_debug") | .data.pairs[]?.reason' | sort | uniq -c
```

示例输出：

```text
    214 fired
     35 controller_withdrawn
     12 no_target_in_range
      8 controller_stuck
```

```sh
# 6b 按塔 × 原因计数（配对数 > 1 时看是哪座塔哑了）
grep '^{' ours.jsonl | jq -r 'select(.event=="night_debug") | .data.pairs[]? | "\(.tower)\t\(.reason)"' \
  | sort | uniq -c
```

示例输出：

```text
     96 30000	fired
     35 30001	controller_withdrawn
     12 30000	no_target_in_range
```

**`night_debug` 的逐回合原始行不要贴进 issue**——它每回合一条，一场 600 行，而 6a/6b 已经把
它压到个位数。本地备查，或者我点名要哪一夜时再跑：

```sh
grep '^{' ours.jsonl | jq -r 'select(.event=="night_debug")
  | select((((.data.round-1)/130)|floor)+1 == 5)     # 第 N 夜，N 自己换
  | [.data.round,.data.robots,(.data.pairs // []|tojson)] | @tsv'
```

---

### 7.4 第四部分：缺口

拿不到的东西**写明原因**，不要静默省略：

```text
receipt.tasks_ours[].score：拿不到——对局详情页只有总分，没有逐 session 分。
receipt.enemy_seen：拿不到——对手基地等级/塔数/墙数在详情页上没有，我方 stdout 也看不见。
表 4b：本场 ours.jsonl 里 errors 全程缺席，不确定是"真没错误"还是"事件没写"。
```

最后一条尤其重要：**"配方命中 0 行"和"这件事没发生过"是两件事**，分不清就说分不清。
早于某次日志改动的批次，配方命中 0 行是正常的（见 §7.5）。

---

### 7.5 第五部分：本轮已改（不必再报）

下面这些**本轮已经改了**，下一份报告不必再把它们当成发现报上来：

- 提交被判错后的重试会**换一种外形**提交（`task_answer_submit.flipped`）；
- `{"status":"pending"}` 这类**包在 JSON 里的哨兵**不再提交给判题器；
- 开拓者在召回前不足 12 回合时**不再接任务**（`task_accept_deferred`）；
- session 连续 15 回合一条都没提交就**提前结束**放开拓者回墙线
  （`task_defense_abort.reason = "sterile"`）。

日志形状的改动（**改动之前抓的批次会在这几处看起来不对，那不是发现**）：

- `sell` 事件**从无到有**（老批次里表 2a 的 `sell` 行命中 0 行）；
- 账本字段名是 **`item`**（`buy` 与 `sell` 同名同形），老批次的 `sell` 行里叫 `ore`；
- `buy` 事件**开始带 `round`**，老批次里它没法对齐到时间线；
- `shopping` 事件**开始带 `round`**，老批次里它没法与 `round.gold` 对齐；
- `night_recall` 与 `night_withdraw` **并入** `night_debug` 的 `pairs[].reason`；
- `round` 里的块（`towers`/`roles`/`wall`/`task`/…）**改为变化才写**，缺键 = 沿用上次的值，
  当回合重写了哪些块由 `round.data.chg` 列出（§2 规则 2）；
- `round` 记录**不再带 `ts`**（回合号就是时钟）；
- `wall_gate_open` **开始带 `away`/`stuck`**（谁没归队、谁走不回岗位），并去掉了固定的
  `reason:"controllers_not_retreated"`；老批次里它只有 `reason`，读不出卡在谁身上；
- `news_legend` **从无到有**（每天一条 `{day, head}`，民间传说的原文）。它一直是宝藏祭坛
  那次 LLM 推断的**唯一输入**，却只存在内存里没落过盘——所以老批次里 `treasure_plan`
  指错了祭坛/祭品/开启日时，无法与"传说被读错了"区分开。老批次里这条配方命中 0 行。

---

## 8. 机器可读指令块

```yaml
agent_request:
  target: workflow-driver
  repo: gamelife1314/Competition
  action: deliver_battle_data
  version: 11
  model: >
    We see the board (per-round state, both sides' units, our own score/gold) but never the
    result. Our process data reaches us only through stdout — anything not written there is
    gone — so the recipes below run over your local capture of it and their OUTPUT is what
    travels. Outcome, identity, version and the six evidence tables (issue_body.parts[2]) are
    yours to paste into the issue. Nothing is asked back: there are no questions to answer,
    only rows to deliver.
  deliverables:
    channel: issue_body
    note: >
      There are no paths. Whatever is not on our stdout is not obtainable, and whatever you
      keep on your own disk stays there — nothing is fetched from your machine. So a
      deliverable is something PASTED INTO THE ISSUE, and the only local artifact that matters
      is the raw stdout capture you run the recipes against. Keep it, keep it re-runnable;
      where it lives is your business. Do not attach it, and do not attach the opponent log
      either: it cannot reach us, and the opponent facts we need ride on receipt.enemy_seen.
    per_battle:
      - paste: receipt.json (one per match_id, never merged)
        content: outcome receipt (see receipt_fields)
        rules: [must parse with jq, every field present, unknown => null]
        budget_lines: 120
    per_batch:
      - issue_header_fields
      - coach summary line
      - the six evidence tables (issue_body.parts[2])
      - budget_lines_total: 400
    local_only:
      desc: yours to keep, never to send
      items:
        - the raw stdout capture (ours.jsonl or whatever you call it) — the recipes' input
        - per-table raw dumps, for when we ask for a specific day rather than an aggregate
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
    - anything unobtainable is named in the gaps part with a reason, never silently omitted
    - all six evidence tables were run, each present even when it matched zero rows
    - no evidence table was replaced by a verdict
    - the six tables together are <= 400 lines (the gate table is the one allowed to run over)
    - no local path appears anywhere in the issue body
    - the issue asks us nothing back
  issue_body:
    desc: >
      What the issue must carry. Five parts, in this order. Nothing here is a question — no
      answers are wanted, only these rows. Exact formatting is free; the fields are not.
    parts:
      - id: batch_overview
        title: 批级概况
        content: the issue_header_fields below
      - id: receipts
        title: 逐场结果回执
        content: one receipt.json per battle (see receipt_fields)
      - id: evidence_tables
        title: 六张证据表
        desc: >
          Run each recipe on the local stdout capture and paste the output. Every recipe below
          is already an aggregate, so the whole output goes in; the all-match run of a
          10-day, 3-tower, 16-session battle is 193 lines / 4.8 KB across all six. Never paste
          only the first few rows, never replace a table with a verdict, and never send a
          path — a file on your disk is not a deliverable. If one table blows past its budget
          either paste it anyway and say why (gate table: the gate WAS open that long) or
          paste the summary recipe instead.
        budget_lines_total: 400
        tables:
          - id: tower_plan
            name: 造塔计划
            tells: why the third tower never went up — mayBuild (weapons) and upgradeReachable (base level) are different doors, and if they take turns being true with nothing built, the block is not money but nobody walking to the site
            recipe: |
              grep '^{' ours.jsonl | jq -r 'select(.event=="tower_plan")
                | "\(.data.towers)\t\(.data.mayBuild)\t\(.data.upgradeReachable)\t\(.data.reserve)\t\(.data.guard)"' | sort | uniq -c
          - id: ledger
            name: 经济台账
            tells: whether frozen gold is an income problem (no sell) or a spending problem (guard reserve blocks the buy) — sell is income, buy is outgo, shopping is "wanted to buy, did we manage"
            recipes:
              - grep '^{' ours.jsonl | jq -r 'select(.event=="buy" or .event=="sell") | [.data.round,.event,.data.role,.data.item,.data.num,.data.gold] | @tsv'
              - grep '^{' ours.jsonl | jq -r 'select(.event=="shopping") | "\(.data.affordable)\t\(.data.need)"' | sort | uniq -c
          - id: task_endings
            name: 任务结局
            tells: timeout and three-strikes are different diseases; a timed-out task is settled on the best answer ever submitted, so reason=timeout does not mean zero
            recipes:
              - grep '^{' ours.jsonl | jq -r 'select(.event=="task_ended") | "\(.data.reason)\t\(.data.success)"' | sort | uniq -c
              - grep '^{' ours.jsonl | jq -r 'select(.event=="task_ended") | [.data.session,.data.reason,.data.success,.data.wrongAnswers,.data.cmdRounds] | @tsv'
          - id: judge_verdict
            name: 判题器裁定
            tells: the only first-hand evidence for a zero-scored task
            error_codes:
              desc: the judger's own coarse buckets (接口文档, Response section)
              0: unknown
              1: task timeout
              2: answer wrong — "incorrect OR incomplete"; a wrong field NAME, a missing field, an out-of-range value and a wrong number all land here
              3: network
              4: command
              5: LLM quota exceeded
            gotchas:
              - do NOT read code 2 as "the value was wrong" — the description is the discriminator
              - errors[] and errorDescs[] are parallel and same-order
            recipes:
              - grep '^{' ours.jsonl | jq -r 'select(.event=="task_answer_submit") | [.data.round,.data.session,.data.chars,.data.flipped,.data.rewritten,.data.wrongSoFar] | @tsv'
              - grep '^{' ours.jsonl | jq -r 'select(.event=="round") | select(.data.errors) | [.data.round, (.data.errors|join(",")), (.data.errorDescs // [] | join(" | "))] | @tsv'
          - id: gate
            name: 封门
            tells: how many rounds the gate stayed open AND who it was waiting on — the dusk check runs every round from dayRound 55 to 69, a window of exactly 15 rounds, so 15 rows in one day means the gate never sealed at all and the ring had a hole all night
            fields:
              away: "[id, x, y] per role that is neither home nor on the operating cells of the tower it mans tonight"
              stuck: "the subset of away that cannot walk to its post at all — walled off from its gun, so the gate will never seal; absent when empty"
            gotchas:
              - the window is 15 rounds (dayRound 55..69), so 15 rows is the maximum, not a count of seal attempts
            recipes:
              - grep '^{' ours.jsonl | jq -r 'select(.event=="wall_gate_seal" or .event=="wall_gate_open") | [.data.round, (((.data.round-1)/130)|floor)+1, ((.data.round-1)%130)+1, .event, ((.data.away // []) | tostring), ((.data.stuck // []) | tostring)] | @tsv'
              - grep '^{' ours.jsonl | jq -r 'select(.event=="wall_gate_open") | (((.data.round-1)/130)|floor)+1' | sort -n | uniq -c
              - grep '^{' ours.jsonl | jq -r 'select(.event=="wall_gate_open") | .data.away[]? | @tsv' | awk -F'\t' '{c[$1]++; last[$1]=$2","$3} END {for (id in c) print id, c[id], last[id]}' | sort -k2 -nr
          - id: night_debug
            name: 夜间塔况
            tells: what each tower spent the night doing; reason carries the whole verdict, and the record is written every round on purpose so that the COUNT of controller_withdrawn is the finding
            vocabulary:
              fired: THE DEFAULT — the tower fired; not an idleness reason
              controller_withdrawn: operator broke contact wounded (row also carries hp)
              controller_walking: recalled and moving back
              controller_stuck: recalled and cannot get back
              controller_healing: operator is taking medicine
              cooldown: the gun is recharging
              no_target_in_range: nothing in reach
              no_target_reserved_for_robots: prey exists but our own trigger discipline is holding fire
            gotchas:
              - '"fired" is the default, so a raw reason histogram mixes the good case in with the causes of silence — filter it out to count idle reasons'
              - 'pairs is pruned away when empty, so iterate with []? or jq fails with "Cannot iterate over null"'
            recipes:
              - grep '^{' ours.jsonl | jq -r 'select(.event=="night_debug") | .data.pairs[]?.reason' | sort | uniq -c
              - grep '^{' ours.jsonl | jq -r 'select(.event=="night_debug") | .data.pairs[]? | "\(.tower)\t\(.reason)"' | sort | uniq -c
      - id: gaps
        title: 缺口
        content: everything unobtainable, with the reason. "The recipe matched zero lines" and "it did not happen" are different claims — say so when you cannot tell them apart.
      - id: already_fixed
        title: 本轮已改（不必再报）
        content: the already_fixed list below
  already_fixed:
    desc: do not re-report these; they changed this round
    items:
      - a rejected answer is retried in the other shape (task_answer_submit.flipped)
      - sentinels wrapped in JSON ({"status":"pending"}) are never submitted
      - no task is accepted with fewer than 12 day-rounds before the dusk recall (task_accept_deferred)
      - a session with nothing submitted after 15 rounds ends and frees the pioneer (task_defense_abort.reason=sterile)
      # log-shape changes: a batch captured before this commit will look wrong
      # in exactly these ways, and that is not a finding.
      - "sell events now exist at all (the ledger recipe used to match zero lines)"
      - "the ledger key is `item` in both directions; an older batch spells it `ore` on sell"
      - "buy now carries .data.round, so it can be placed on the timeline"
      - "shopping now carries .data.round, so it joins against round.gold"
      - "night_recall and night_withdraw are folded into night_debug's pairs[].reason"
      - "round blocks (towers/roles/wall/task/...) are change-gated; absent means unchanged, and round.data.chg names what was re-sent"
      - "round records no longer carry ts (the round number is the clock)"
      - "news_legend is new: one {day, head} a day with the folk legend's own words, which are the sole input to the altar inference and used to live only in memory — an older batch cannot tell a misread legend from a wrong plan"
```

---

## 9. English summary

We see the board, never the result. Everything the per-round request carries is ours to log,
and we do: `/docs/WORKFLOW_REQUEST.md` v11 asks you to (1) keep a raw capture of our stdout,
(2) run six recipes over it, and (3) paste the batch header fields, one outcome receipt per
battle, and the six tables into the issue.

**The channel is the issue body, and nothing else.** Anything not on our stdout is
unobtainable, and anything on your disk stays on your disk — so there are no paths to give
and no files to attach. The one local artifact that matters is the stdout capture itself:
keep it re-runnable, because the tables are derived from it and we may ask for a specific
day rather than an aggregate. Do not attach it, and do not send the opponent log either.

1. **Keep the capture whole** — no truncation, no merging, no redaction, line order
   preserved. The first stdout line is not JSON. Rounds ≠ lines.
2. **Per-battle receipt** (one per `match_id`, never merged) — the fields we cannot observe
   from inside: winner, both totals, the task/kill/survival split, opponent identity, the
   actual `base_commit`, the per-session task scores, and the `CG_*` values the battle ran
   with. Unknown values must be `null`, never `0` — a fabricated zero is worse than a missing
   one, because we will attribute a change to it.
3. **Issue header** — per-battle commit, batch size (≥5 battles / ≥2 opponents), rank,
   cumulative record, win rate, one line per battle, and the fixed-format coach summary line.
4. **Six evidence tables** (§7.3) — tower_plan, the buy/sell/shopping ledger, task_ended
   endings, the judger's verdicts, the wall gate, and night_debug. Every recipe is already an
   aggregate: all six together come to 193 lines / 4.8 KB on a 10-day, 3-tower, 16-session
   battle, against a 400-line budget. **This is the part we need most**: you can see the
   result and we cannot, so these tables are the only process data that reaches us. A table
   replaced by a verdict is worth nothing, and "the recipe matched zero lines" is not the same
   claim as "it did not happen".
5. **Self-check** (§6) — run it before publishing; it catches missing fields, a wrong
   `base_commit`, count mismatches against our own log, a table gone over budget, and a local
   path left in the body.

**There are no questions to answer.** v9 removed the reverse-questioning of v8 (§7.1's six
mechanism questions and §7.2's three open questions): deliver the rows, and the analysis is
ours to do.

The `coach` block is extracted from our JSONL with the jq recipes in §5; the coach itself
needs no environment variables and no control group from you.

---

*本文件由本地 agent 维护，作为向内部 workflow 下发数据交付指令的接口。*
