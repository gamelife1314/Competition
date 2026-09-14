# 对战数据交付规范（workflow agent 接口）

> **版本 v21** · 本文档是内网自动对战 workflow 的**交付规范**。
> **v21 只加一节：§19 请求十四**，别的什么都没动——交付方式、**收集命令一个字没变**，
> 你那边**不需要做任何新动作**。§19 要的表 11（买家出门账）是**聚合式、恒定二十行**，
> 与表 2b 共用同一份预算（`CAPS` 总和 676 不变）；起因是 2026-09-15 那五场
> （issue #156-#160），五个对手、五场 0:3，而 v20 问的那个问题**这一批已经有答案了**：
> 买得起却没买，既不是没走到第 7 步、也不是背包满，而是**这一趟来回走不完**——
> 买家同时是炮位操作手，商店是地图上最远的差事，旧的 `buyer_flow` 从不比较
> 「往返要多少回合」和「离黄昏还剩多少回合」。代码已改（§19.4），§19 要的是
> **验收它的那一格**：表 11 的 `no_time` / `walk` 两行。
>
> **版本 v20** · 本文档是内网自动对战 workflow 的**交付规范**。
> **v20 只加一节：§18 请求十三**，别的什么都没动——交付方式、**收集命令一个字没变**，
> 你那边**不需要做任何新动作**。§18 要的是日志里**已经有、但收集脚本没印**的三格
> （买家的 `free_slots` / 为什么没走到第 7 步 / 该回合是不是被 `should_sell` 顶掉），
> 所以这一版**连行数预算都没动**（`CAPS` 总和 676 不变，新增的表 11 是聚合式、恒定二十行，
> 从表 2b 里换，不是加）。
> 起因是 2026-09-15 那十场（issue #161-#170）：十个对手，**十场 0:3**，而表 2a 里
> **一笔 `StationUpgradeVoucher1` 都没有**——十场里九场的基地倒了。这一批已经把
> 那个结构性的抢钱顺序修了（§18.4），但表 2b 只印**队首**那一个需求，所以
> 「队首当回合买得起、却一笔都没买」这件事在表里看不出来：issue #170 有 30 个回合
> 的队首是 `WeaponUpgradeVoucher1` 且 `affordable=true`（金币峰值 130-146，
> 表 2c），整场却没买过一次。是买家没走到第 7 步、是背包满了（`free_slots=0`）、
> 还是被 `should_sell` 顶掉了——**这三件事现在长得一模一样**，而它们要改的地方
> 完全不同。另外这一版顺手修了收集脚本自己的一个读数 bug（§18.4 第三条）：
> 表 0 / 表 8 的「我方基地」列在十份报告里九份是空的。
>
> **版本 v19** · 本文档是内网自动对战 workflow 的**交付规范**。
> **v19 只加一节：§17 请求十二**，别的什么都没动——交付方式、**收集命令一个字没变**，
> 你那边**不需要做任何新动作**：重跑一次那条命令就会多印一段**表 10（天亮清除账）**，
> 表 4a 会多一列「外形」。行数预算 700 **不变**（表 10 十二行，总预算 664 → 676）。
> 起因是 2026-09-14 那五场（issue #131-#135）：**又是五场 0:3**，而表 8 的
> `residual` 连续五批被读成「任务线在丢 106-409 分」——**这个读法是错的**。
> `residual = 总分 - kill - survival`，而 `kill` 是**我们自己数的**：任务书 4.7.3
> 规定残余机器人天亮自动清除，我们把「清除」也数成了「击杀」，于是每早把整晚的
> 残余机器人计一次分，差额全部落进 `residual`。任务书 ch.6 的
> `score_1 = 奖励 × 通过率`，两个因子都不为负，它不可能变成 -409。
> 这一批已经在代码里把天亮和第 4.7.2 节的击杀分开（§17.4），但**判题器到底算不算
> 这部分分，现有表里没有一格能证明**——§17 就是补那一格。同时补表 4a 的「外形」列，
> 因为这一批改了首次提交的答案包装规则，没有这一列验收不了。
>
> **版本 v18** · 本文档是内网自动对战 workflow 的**交付规范**。
> **v18 只加一节：§16 请求十一**，别的什么都没动——交付方式、**收集命令一个字没变**，
> 你那边**不需要做任何新动作**：重跑一次那条命令就会多印一段**表 9（逐 session 分数
> 归属）**。行数预算 680 → **700**。
> 起因是 2026-09-14 那五场（issue #126-#130）：**又是五场 0:3**，而表 8 给出的答案是
> 「**我方基地在夜 1-夜 3 被打爆**」——五场里四场活不过第二夜，`scoreAttr.survival`
> 全程停在 0-30，而应得是 100-150。基地一倒 `score` 就冻住（#130 停在 102，#127 停在
> 53），后面四天的击杀分全部不计。所以这一批的两处改动都在防守侧（见 §16.4），而它们
> **没法只靠现有的表验收**：表 8 的 `residual` 是**全场累计**的，一场里改了任务线、
> 分数涨在一个 session 上，累计列上看不出来；城墙被修回来多少血，现在**一格都没有**。
> §16 就是把这两件事补成可直接读的两列。**收集命令一个字没变**，照旧：
> `python tools/collect_log.py <日志文件>`。
>
> **版本 v17** · 本文档是内网自动对战 workflow 的**交付规范**。
> **v17 加两节：§14 请求九 + §15 请求十**，别的什么都没动——交付方式、**收集命令一个字
> 没变**，你那边**不需要做任何新动作**：重跑一次那条命令就会多印一段**表 8（分数拆解）**，
> 而表 6b 的「原因」列会自己带上卡位坐标。行数预算 660 → **680**（只多了表 8 的 20 行）。
> §15 起因是 #125 有一个角色整夜 15 回合钉在同一格 (30,13)、那门炮一整晚没开火，
> 而日志里只有 `controller_stuck` 三个字，读不出为什么。
> 起因是 2026-09-14 那五场（issue #121-#125）：**五场全是 0:3**，我方 31/34/47/54/152 分，
> 对手 488/1065/645/203/1151 分。表 7（v16 刚加的那张）在这五场里印出来的是同一个数：
> **对方塔数 = 0，每一天都是 0**，而对手的"击杀分"从 49 一路涨到 577。也就是说
> **一座塔都不造的对手，积分是我们的一到三十倍**。这一格现在能读出来，但**它是什么
> 组成的读不出来**——§14 就是要把这 577 分拆开，好知道下一批该往哪儿使劲。
>
> **v16** · 本文档是内网自动对战 workflow 的**交付规范**。
> **v16 只加一节：§13 请求八**，别的什么都没动——交付方式、**收集命令一个字没变**，
> 你那边**不需要做任何新动作**，重跑一次那条命令就会多印一段**表 7（对手建造节奏）**。
> 行数预算 640 → **660**（只多了表 7 的 12 行）。
> 起因是这一批的 Part B：老板看了比赛，说"**对手优先造武器，我们优先造墙**"。这条
> 观察的对手那一半，`receipt.enemy_seen`（§3）报过两次"拿不到"。**前半句对，后半句
> 不对**：`teamEnemy.roles` 里一直带着对手的所有单位，我们的 `round` 记录里也一直写着
> `enemyWall` 和 `enemyBase`——缺的只是 `enemyTowers` 那一格，这一版补上了。于是
> "对手第几天有几门炮"现在能从**我们自己的日志**里读出来，不再依赖那个拿不到的回执
> 字段。§13 说明这一格为什么能定 Part B（我们实测：前两座塔 R2、第一块墙 R16、第 3 座
> 塔要到第 2 天；而把第 3 座塔提到第 1 天，代价是环上留一个洞）。
>
> **v15** · 本文档是内网自动对战 workflow 的**交付规范**。
> **v15 加了一节：§12 请求七**，并把 §7.3 / §8 的行数预算从 560 提到 640——因为 §7.3
> 从**十三张表**变成了**十六张**：多了表 0（分数归属）、表 4c（每条沙盒命令）和表 4d
> （没有答案的原因）。
> 起因是 2026-09-14(b) 那五场：每场 8-13 条 `task_cmd_failed`，字段只有
> `{"exit":0,"timeout":false}`，被读成"沙盒坏了、命令执行失败"整整两批——实际是**脚本
> 跑完了、没打印 `ANSWER:` 行**。而同一场的 `cmd_result` 只有字符数，没有一行说得清脚本
> 到底返回了什么，于是"为什么一个答案都没交上去"在日志里无法证伪。这一版把两个字段
> （`cmd_result.answer`/`head`、`task_cmd_failed.reason`/`streak`）补上，并让收集脚本
> 直接把它们印成表；同时把一直躺在 `round.scoreAttr` 里、却从没印过的**分数归属**
> （`score_1`/`score_2`/`score_3`）印成表 0。**收集命令一个字没变**，照旧：
> `python tools/collect_log.py <日志文件>`。
>
> **v13** · 本文档是内网自动对战 workflow 的**交付规范**。
> 读者：拉代码 → 发起对战 → 抓日志 → 分析对局 → 生成改进 issue 的 code agent。
> **v13 不改交付方式，只加一节：§10 请求六。** 2026-09-14 那批 P0（任务反馈环、第 3 塔
> 排序、墙优先、不把闲人封死在外）落地后，有三处**观测缺口**浮出来：表 1 从现在起会把
> "按墙优先主动压住第 2/3 塔"读成"没人走到工地"，表 5c 的 `stuck` 对"无塔可守"的角色永远
> 为假，而 P0-1 接回去的那段判题器原话落在 prompt 末尾、`prompt_sent.head` 只记头 300
> 字符——**永远看不到**。三件都写清楚了是什么/为什么/建议格式，你们照旧只交数据、不必
> 回答。另外 §7.5 里补了 v13 的行为改动，下一批不要把设计好的顺序当成新缺陷再报一遍。
> **v12 的规矩没变：跑得动。** 内网 agent 在 **Windows** 上，jq / awk / grep 不保证有
> ——jq 恰恰是 Git for Windows 唯一不自带的那个。所以 §7.3 的六张表换成了**一个 Python
> 脚本**（`tools/collect_log.py`，只用标准库，不需要 pip install），三个平台同一条命令：
> `python tools/collect_log.py <你的日志文件>`。输入 `.jsonl` / `.jsonl.gz` / `.zip` 都行，
> BOM / CRLF / UTF-16 / GBK 都能读；跑不了 Python 3 就照 §7.4 写一句"跑不了 + 报错原文"。
> v11 定的通道没变：没有文件路径可用，我拿不到你机器上的任何东西——**issue 正文就是
> 交付物**，§8 的 `deliverables` 因此是 `paste:`；对手日志与"原始行 tsv"两项不再要。
> §0 是新的：**只做三件事**（每场一行胜负 · 跑那条命令 · 缺口写一句），做不完就只做这
> 三件——§1 的 P0/P1/P2 优先级依据在这里。v10 把表 5（封门）加厚了：`wall_gate_open`
> 现在带 `away`/`stuck`，能直接读出**门卡在谁身上**，并且明确了"这个窗口总共只有 15
> 回合，15 行 = 整夜没封"。§7 是正面清单（issue 正文要给我哪五部分、怎么取、长什么样，
> 全部给示例），你不需要回答我任何问题，把六张表填出来交给我是唯一的要求。§6 是你可以
> 自己跑一遍的自检清单；§8 是同一份内容的机器可读版本。

---

## 0. 最短路径：只做三件事，我就能干活

每批就这三件。做不完就只做这三件，其余照 §7.4 写一句"没拿到"。

```text
0. 前提只有一个：机器上有 Python 3。`python --version` 印出 3.x 就行。
   印不出来（或弹出应用商店）就先装一个，然后**重开一个终端**：§7.3 有各平台的安装命令。
1. 每场一行结果：对局ID + 代码版本 + 胜负 + 比分      ← 从对局详情页抄
2. 每场跑一条命令，把整段输出贴进来（cmd / PowerShell / bash 都一样）：
     python tools/collect_log.py <你的日志文件>
3. 拿不到的东西，写一句"拿不到 + 为什么"
```

| 做什么 | 为什么是它 | 做不了怎么办 |
|---|---|---|
| 第 1 件：胜负与比分 | **唯一的硬数据**。没有它，"这轮改动是不是真的变好了"只能靠猜——我连输赢都看不见 | 详情页没有就写 `null`，**不要填 0**（§3.4） |
| 第 2 件：那条命令的输出 | **唯一的现场证据**。棋盘我看得见，过程只有日志记得；而这条命令把日志压成一页 | 命令跑不了就说"跑不了 + 报错原文"，别自己另编一张表 |
| 第 3 件：缺口 | "表印出（0 行）"和"这件事没发生过"是两件事，分不清就说分不清 | —— |

**做得更多当然更好**：§1 是完整清单，§3 是 15 个字段的完整回执，§7 是五部分正文。
但那些是**加分项，不是及格线**。两件不要做的事：**不要填猜的数**（一个编的 `score` 比
`null` 更糟，因为我会拿它去归因）；**不要贴没跑过的表**（一行都没跑就说没跑，别用"结论：
正常"顶上）。

**交付通道只有一条：issue 正文。** 你机器上的路径我一个都拿不到——文件下载到本地就停在
本地了。所以"交付"= 把内容**贴进 issue**，不是"把文件放在某个目录里"（§1）。

分界线只有一条：**棋盘我看得见，结果我看不见。**

每个回合的请求里带着：回合号、地图、我方队名/ID/金币/总分、我方任务板、**双方所有
单位的血量与等级**、机器人、两家商店、判题器的错误码与原文。但 `teamEnemy` 里**只有
`roles`**——对方的总分、金币、任务提交，以及**这一场谁赢了**，我一律看不到。

所以这件事分成两半，各归各管：

| 半边 | 谁提供 | 怎么到我手里 |
|---|---|---|
| 逐回合过程数据 | 我（stdout JSONL） | 你留着**没加工过的那份**，跑 §7.3 那一条命令，把**输出**贴进 issue |
| 结果、身份、版本 | 你 | 按 §3 的字段填进回执，贴进 issue |

---

## 1. 交付物清单

下面是完整清单，按**优先级**排。`P0` 三件是 §0 那三件，其余是加分项——**做完 P0 再谈
P1，别把 P1 做成半成品**。

| 优先级 | 内容 | 从哪来 | 做不成的代价 |
|---|---|---|---|
| **P0** | 每场一行结果：`对局ID 代码版本 胜负 比分` | 对局详情页 | 我无法判断任何改动的效果。**这一项缺失，整批数据基本作废** |
| **P0** | **一条命令的输出**：`python tools/collect_log.py <日志>` 整段贴 | 你的日志文件 | 我失去全部现场证据，只能看结果猜过程 |
| **P0** | §7.4 缺口：拿不到的和原因 | 你自己 | 我会把"没写"读成"没发生" |
| P1 | §3 的完整回执（15 个字段），一场一份 | 对局详情页 | 分项得分、逐 session 任务分、对手身份拿不到——归因会变粗 |
| P1 | §4 的 8 个头部字段 | 平台页面 + §5 的 `--coach` 输出 | 看不出跨批次的趋势（排名、胜率） |
| P2 | §5 教练汇总一行 | 你的日志 | 看不出内置教练移动过开关没有 |
| P2 | §7 的第一、二、五部分（批级概况 / 逐场回执正文 / 本轮已改） | 汇总上面几项 | 组织性损失，不是信息损失 |

**`tools/collect_log.py` 就是 §7.3 那六张表本身**——筛哪些事件、怎么聚合、每张表印多少行
都写死在脚本里，所以你不用判断"贴多少才够"。它只要能跑 Python 3（Windows 上直接
`python tools\collect_log.py <日志>`，不需要 jq / awk / bash）。真跑不了就照 §7.4 写清原因；
**不要自己另编一张表**。

**原始日志你自己留着。** 存在哪、叫什么、怎么组织都行，也不必在 issue 里报路径——**只要保证
随时能重跑那条命令**，因为表里每一行都是从它跑出来的。我问"某天黄昏门卡在谁身上"的时候，
你要能当场再跑一次贴上来。

**不再要的两项**（v10 及更早的清单里有，现在取消）：

- **对手日志 `enemy.jsonl`**。它到不了我手里，而我要的那点对手事实（基地等级 / 塔数 / 墙数）
  已经在 `receipt.enemy_seen` 里了。拿不到也不必写 `.missing`，§7.4 提一句就够。
- **"原始行另存 tsv + 给路径"**。路径对我无效。原始行留在本地备查，贴的是**脚本的输出**。

每批：issue 头部 8 个字段（§4）+ 每场一份 `receipt.json`（§3）+ 一行教练汇总（§5）+
**§7.3 的六张证据表**（整个 issue 的正文就是 §7 那五部分）。

---

## 2. 我方日志：本地那份要整份留着，别加工

> 这一节讲的是**你本地保存的那份原始 stdout**。它不进 issue（太大），但 §7.3 那条命令跑在
> 它上面——所以它一旦被截断、被合并、被"顺手清理"，表就跟着失真，而我无从察觉。
> 贴进 issue 的是命令的输出，不是这个文件。

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
- 本地怎么存都行（gzip、按场分目录、随便命名），**前提是重跑那条命令时能还原成原始行序**。

**三个必须知道的压缩规则**（不知道就会把"没写"读成"没发生"）：

1. **空值不落盘。** `null`、`""`、`[]`、`{}` 一律不写 —— 缺键和空值同义。但
   **`0` 和 `false` 是事实，照写**：`gold:0` 是没钱，`noRobotDamage:false` 是对本回合的
   断言。所以"有错的回合"就是记录里**有 `errors` 键**的那些，而 `gold` 为 0 的回合
   照样带着 `gold: 0`。
2. **没变的块不重写。** `round` 里的 `stationHp`/`enemyStationHp`/`wall`/`enemyWall`/
   `towers`/`roles`/`pairs`/`task`/`treasure` **只在变化的那一回合写**，其余回合整个键
   缺席 —— **缺键 = 沿用上一次出现的值，不是"没有"**。那一回合重写了哪些块，由
   `round.data.chg` 列出；**没有 `chg` 就是本回合无变化**。
   **`chg` 是权威，键不是**：块名在 `chg` 里、键却不在记录里，意思是这个块**变成了空的**
   （塔全被拆光、配对清空、任务会话结束）—— 空值本身不落盘（规则 1），所以由 `chg` 宣布。
   只看键的读者会在这一回合继续沿用旧值，错得无声无息。**要取序列必须先"带值前行"**：
   某回合没有 `towers` 键，就用上一次出现过的 `towers`，直到它再次出现为止——表 1、表 5a、
   表 6a 都是这么读的。
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
ID/坐标、只保留 `round` 事件、只保留你分析里引用到的那几行。这些都让表在你手上和在我
手上长得不一样，而我看不出来。

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
| `coach` | object | 从我的日志里取，见 §5 | 必填 |

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
| 教练汇总 | 见 §5 | 格式固定，我按它对齐 |

排名与胜率是判断"改进是否真的有效"的唯一长期指标：单场胜负可能是运气（对手强弱、
出生点、地图随机），只有趋势能证伪。

---

## 5. 教练记录：回执里的 `coach` 那一块

教练是决策器内置的在线自适应（`CoreGeek/src/brain/coach.rs`），事件有四种：
`coach_ready`（启动时的档位）、`coach_night`（每夜结算）、`coach_move`（**移动了一个
开关**）、`coach_half`（半场结束）。另外逐回合的 `round.data.policy` 记录**当时**档位。

**这一块也由那条命令给你**，加一个 `--coach` 就行——它印出的三行正好是回执里要填的东西：

```sh
python tools/collect_log.py <你的日志文件> --coach
```

输出长这样（第二行整段贴进 `receipt.json` 的 `coach.moves`）：

```text
receipt.coach.moves（整段贴进 receipt.json 的 coach.moves）：
[{"round":261,"day":3,"switch":"harass","from":"Rhythm","to":"Off","why":"summons_sterile"}]
receipt.coach.halves：1（最后一个 coach_half）
本场汇总：移动 2 次（harass 1 / station_pressure 1）
```

**没动过就是 `[]` 和 `0`，照实写**——「一次没动」是个结论，不能空着（空着我会读成"没记"）。

两个坑：`coach_move.from` / `to` 是**字符串**（布尔档位写作 `"true"` / `"false"`，
`harass` 是档位名 `Off` / `Rhythm` / `Rich`）；而 `round.data.policy.stationPressure`
是**布尔**。同名字段两种类型，别用一套解析。

issue 头部请给这一行（每场的「本场汇总」连起来就是这个，N / X / W / L / D 数一下）：

```
教练：本批 N 场 | 移动 M 次（station_pressure a / gap_funding b / harass c） | 至少移动过一次的场次 X/N | 结果分布 胜 W / 负 L / 平 D
```

---

## 6. 交付前自检

每一场都跑一遍，任何一条不过就别发：

1. receipt.json 能解析，且键名覆盖 §3.3 的**全部**字段：
   `python -c "import json;print(sorted(json.load(open('receipt.json',encoding='utf-8'))))"`
2. 收集器开头那行 `事件计数：` 里的 `round=` ≥ 该场回合数（只会更多，不会更少）。
3. `git cat-file -t <base_commit>` 返回 `commit`，且时间与该场吻合。
4. `receipt.coach.moves` 条数 == `--coach` 印的 `本场汇总：移动 M 次` 里的 M。
5. `receipt.coach.halves` == `--coach` 印的 `receipt.coach.halves：` 后面那个数。
   该行括号里写了这个数是怎么来的（最后一个 `coach_half`，一场都没有就是 `0`）——
   照抄，别自己另算一个。
6. 没有把 `null` 写成 `0` / `""` / `"unknown"`；`result` 之外的字段出现 `"unknown"` 一律算错。
7. 对手那一侧拿不到的东西，在 §7.4 写明原因，而不是静默省略。
8. §7.3 的六张表**是表，不是结论**。一张命中 0 行的表就让它印「（0 行）」——"表 6a 结论：
   正常"和一张空表是两回事，前者我看不出你有没有跑过。
9. issue 正文里**没有**任何"请你回答"式的反向提问——不存在这样的问题。
10. 表 5 命中 15 行的那天，写的是"**整夜没封**"而不是"开了 15 回"；`away`/`stuck` 里出现的
   角色 id 在表里点名了——门卡在谁身上是这张表存在的理由。
11. 六张表是**一条命令**跑出来的（`python tools/collect_log.py <日志>`），整段贴，没有手工删改；
    被截断的节带着「本表共 N 行」的声明。**不用自己数行数——脚本已经封顶了。**
12. **issue 正文里不出现任何本地路径**。写给自己看就行：路径到我这里是死字，我打不开。

---

## 7. issue 正文：五部分

放进 issue 的信息就是这五部分。前两部分是你对平台数据的转写，第三部分是从 `ours.jsonl`
跑出来的**六张表**，第四部分是取不到的东西，第五部分是"别再报这个"。

**格式不必和我一样，字段和内容要在。** 每部分都给了取法和一段示例输出。
示例里的数字是编的，只为说明表的形状——不要把它当成任何一场的实际数据。

---

### 7.1 第一部分：批级概况

§4 的 8 个字段。来源：平台页面 + §5 的 `--coach` 输出。

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

**一条命令，整段贴。** 不用自己筛事件、自己聚合，也不用判断贴多少：

```sh
python tools/collect_log.py <你的日志文件>        # .jsonl / .jsonl.gz / .zip 都行
```

Windows 上就这么写，不用改斜杠（cmd / PowerShell / Git Bash 都认）。
`python --version` 印不出 3.x 就先装：Windows `winget install Python.Python.3.12`
（或 python.org 下载安装包），macOS `brew install python3`，
Debian/Ubuntu `sudo apt-get install -y python3`；装完**重开终端**再跑。
如果 `python` 一跑就弹出应用商店，那是 Windows 的占位符——换 `py tools\collect_log.py <日志>`。
脚本只用标准库，**不需要 pip install**。

它的输出就是这一节要的六张表，一节一块，带列名，某张表命中 0 行会明写「（0 行）」而不是
消失，超长会截断并写明「本表共 N 行」。**把整段输出贴进 issue 即可**，一个字都不用改。

跑不了这条命令（没有 Python 3）就照 §7.4 写"跑不了 + 报错原文"——**不要自己另编一张表**，
也不要用"结论：正常"代替表。**结论由我来下，你给我行。**

**下面讲的是这六张表各是什么、我怎么读它。这里没有需要你跑的命令。**每张表的实现（筛哪些
事件、怎么聚合、封顶多少行）都在 `tools/collect_log.py` 的 `build_tables()` 里，按 `表 N`
分段，要核对某一格是怎么算出来的就看那儿。

- **每张表都有行数预算**（脚本按这个封顶）。"实测"是拿一场 10 天 / 3 座塔 / 16 个 session
  的对局按真实批次的形状跑出来的：**整段贴出来 250 行 / 7.0 KB**，其中表格 219 行、表头 16 行。

| 表 | 脚本里的节 | 实测 | 脚本封顶 | 增长源 |
|---|---|---|---|---|
| 0 分数归属 | 0 | 10 | 12 | 一天一行 |
| 1 造塔计划 | 1 | 3 | 20 | 聚合式，恒定 |
| 2 经济台账 | 2a / 2b / 2c | 33 / 2 / 50 | 60 / 20 / 60 | 钱动了多少回 |
| 3 任务结局 | 3a / 3b | 2 / 15 | 20 / 40 | session 数 |
| 4 判题器裁定 | 4a / 4b / 4c / 4d | 14 / 8 / 14 / 3 | 40 / 40 / 40 / 20 | 提交次数 / 出错回合数 / **沙盒命令数** |
| 5 封门 | 5a / 5b / 5c | 52 / 3 / 1 | 160 / 20 / 20 | **每天最多 15 行** |
| 6 夜间塔况 | 6a / 6b | 3 / 7 | 20 / 40 | 聚合式，恒定 |
| 0b 分数拆解 | 8 | 3 | 20 | 一天一行，**v17 新增** |

**封顶在脚本里，不用你数**：跑一次最多 680 行表格（那是"每天都出问题"的极端场），典型一场
250 行 / 7.0 KB。被截断的节会自己写明「本表共 N 行」——**看到那句就照贴，我会知道哪节被切了**。

- 表 5 的 5a 是唯一按回合线性长的：黄昏窗口一天就 15 回合，所以**每天封顶 15 行**，10 天
  150 行加 10 行封门，正好是它 160 的上限。它长不是"贴多了"，是"门开太久了"——正是我要看的。
  真想省地方就先贴 5b（一行一天），我点名要哪天的 5a 再补。
- 不要只贴头几行，也不要用"结论：正常"代替表格——结论由我来下，你给我行。
- 表下可以写一两句你怎么读它，但**行必须在**。

---

#### 表 1 · 造塔计划 `tower_plan`

**我用它判断**：第三座塔为什么没造出来。`mayBuild`（手里的钱够不够买武器）和
`upgradeReachable`（够不够升级基地）是两个不同的门，如果它们轮流为真却谁也没落地，
卡点就不在钱上，而在"没人走到工地"。

示例输出（第一列是回合数，后面依次是 塔数 / mayBuild / upgradeReachable / reserve / guard / 外层缺口）：

```text
     18 2	true	false	25	false	0
     34 2	false	true	25	false	0
      6 3	false	false	25	true	4
```

> 末列 `外层缺口`（`secondLayer`）是 v14 的 P2-1：本回合 `wallGaps` 里有几格是**第二层**
> （环 3）而不是那圈挡夜的主环。它只在主环闭合、三塔到位、且某个扇区真的挨过打之后才非零，
> 每天最多 4 格。**读法**：主环还差格时这一列恒为 0（外层墙是主环闭合之后才买的），
> 所以 0 不等于"这条线没做"，`wall_build` 事件里的 `layer` 才是它到底动没动工。

**逐回合的原始行不要贴进 issue**——它是 700 行，这一节的预算只有 20 行，脚本自己会截。
留在本地，或者我点名要哪一天时再跑（`--day` 的 `N` 自己换）：

```sh
python tools/collect_log.py <你的日志文件> --day 5      # 把 5 换成我要的那一天/夜
```

`--day N` 照样把六张表一起印出来，只是**末尾多几节第 N 天的逐回合明细**（标着 `drill`）。
**整段贴，别只贴 drill 那几节**——六张表是每批都要的，我这里只是额外要了那一天的原始行。

---

#### 表 2 · 经济台账 `buy` / `sell` / `shopping`

**我用它判断**：金币冻结是**收入端**没进账，还是**支出端**被守卫金挡住。三个事件要一起看：
`sell` 是进账，`buy` 是出账，`shopping` 是"想买但买没买到"（`affordable` 为 false 的回合占比）。

示例输出：

```text
5	sell	10002	iron	4	25
9	buy	10002	WallFixer	1	0
23	sell	10003	iron	4	25
31	buy	10002	WallFixer	1	0
```

示例输出：

```text
    612 false	WeaponUpgradeVoucher1
    140 true	WallFixer
     44 false	WeaponUpgradeVoucher1
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
python tools/collect_log.py <你的日志文件> --day 5      # 把 5 换成我要的那一天/夜
```

---

#### 表 3 · 任务结局

**我用它判断**：12 个 session 全 0 分，"超时"和"判错三次"是两种完全不同的病——
超时结束时判题器按此前提交过的最好答案结算（任务书 `timeoutRounds` 条），所以
`reason=timeout` **不等于** 0 分。

> **v14 起 3b 有两个"次数"列，别只看一个。** `拒绝次数`（`rejections`）是单调的：
> 这个 session 一共烧掉几个答案。`判错次数`（`wrongAnswers`）是**放弃计数器**——判题器
> 说出一个此前没说过的新错因（`MissingNamedInput: city` 之后又来一个 `$/token: 缺少键`），
> 它就归零（P1-1）。所以 `拒绝次数=4 判错次数=0` 读作"被拒四次，但每次错因都是新的，
> 重试是对的"，**不是**"第一次提交"。只印 `判错次数` 会让这种 session 看起来没被拒过。

示例输出（3a 结局直方图）：

```text
      7 timeout	true
      3 timeout	false
      2 wrong_answers	false
```

示例输出（3b 逐 session：session 原因 成功 拒绝次数 判错次数 回合数）：

```text
1	timeout	true	3	0	37
2	timeout	true	0	0	22
3	wrong_answers	false	5	3	58
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

示例输出（回合 / session / 字符数 / 换过外形 / 被改写 / 判错几次 / 拒绝几次 / 带回错因）：

```text
48	1	212	false	false	0	0	0
61	1	208	true	false	1	1	1
77	2	96	false	false	0	2	2
```

> 末两列按 §10 请求六之三补上：`带回错因` = 这次重试的 prompt 里带了几条判题器原话。
> 它是"判题器的原话到底有没有进 prompt"唯一能证伪的一格——`0` 意味着这一轮的反馈环
> 是断的，那么这一格的 `判错` 就不能算在"重试策略"头上。`判错`/`拒绝` 两列的区别见 3b
> 上面那段（前者会归零，后者单调）。

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

示例输出（回合 / 第几天 / 当天第几回合 / 事件 / 没归队的 / 走不回去的）：

```text
185	2	56	wall_gate_open	[[10004,13,26]]	[]
186	2	57	wall_gate_open	[[10004,13,26]]	[]
187	2	58	wall_gate_open	[[10002,33,13]]	[10002]
188	2	59	wall_gate_seal	[]	[]
```

示例输出：

```text
      3 1
     15 2
      2 4
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
> 不落盘**（§2 规则 1），所以取 `pairs` 时要容忍它整个缺席。
> `Cannot iterate over null`。

示例输出：

```text
    214 fired
     35 controller_withdrawn
     12 no_target_in_range
      8 controller_stuck
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
python tools/collect_log.py <你的日志文件> --day 5      # 把 5 换成我要的那一天/夜
```

---

### 7.4 第四部分：缺口

拿不到的东西**写明原因**，不要静默省略：

```text
receipt.tasks_ours[].score：拿不到——对局详情页只有总分，没有逐 session 分。
receipt.enemy_seen：拿不到——对手基地等级/塔数/墙数在详情页上没有，我方 stdout 也看不见。
表 4b：本场 ours.jsonl 里 errors 全程缺席，不确定是"真没错误"还是"事件没写"。
collect_log.py：跑不了——机器上没有 Python 3，报错原文：`python` 不是内部或外部命令；六张表这次没跑。
```

最后一条尤其重要：**"表印出（0 行）"和"这件事没发生过"是两件事**，分不清就说分不清。
早于某次日志改动的批次，某张表是（0 行）是正常的（见 §7.5）。

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
  指错了祭坛/祭品/开启日时，无法与"传说被读错了"区分开。老批次里这张表是（0 行）。

**v13（2026-09-14 的 P0 落地）改的是行为，不是日志形状，但它会改变表 1、表 4 的样子**
——不写在这里，下一批会把设计好的顺序当成新缺陷再报一遍：

- **第 3 塔在第 1 天常常"迟到"**：墙优先闸生效后，第 1 天墙环还没成型时，第 2/3 塔会被
  **主动跳过**（首塔 gatling 豁免）。表 1 的 `mayBuild` 因此可能出现"为真但两塔不动"的长段，
  那不是"没人走到工地"（见 §10 请求六之一）；
- **第 3 塔一经买得起就会建**：`may_build_weapon` 不再为 100 金的升级券扣住金币，所以
  D2 起"金币到 25 就掉回 0"是正常的（表 2c 会看到这个台阶），不再等于"经济被冻结"；
- **重试的 prompt 变了**：提交被判错后，prompt 末尾会带判题器的原话反馈（见 §10 请求六之三），
  所以表 4b 的同一类拒绝不该再连续出现三次同一种错法——若仍然出现，那才是发现。

---

## 8. 机器可读指令块

```yaml
agent_request:
  target: workflow-driver
  repo: gamelife1314/Competition
  action: deliver_battle_data
  version: 13
  model: >
    We see the board (per-round state, both sides' units, our own score/gold) but never the
    result. Our process data reaches us only through stdout — anything not written there is
    gone — so the tables below run over your local capture of it and their OUTPUT is what
    travels. Outcome, identity, version and the six evidence tables (issue_body.parts[2]) are
    yours to paste into the issue. Nothing is asked back: there are no questions to answer,
    only rows to deliver.
  deliverables:
    channel: issue_body
    note: >
      There are no paths. Whatever is not on our stdout is not obtainable, and whatever you
      keep on your own disk stays there — nothing is fetched from your machine. So a
      deliverable is something PASTED INTO THE ISSUE, and the only local artifact that matters
      is the raw stdout capture you feed the collector. Keep it, keep it re-runnable;
      where it lives is your business. Do not attach it, and do not attach the opponent log
      either: it cannot reach us, and the opponent facts we need ride on receipt.enemy_seen.
    per_battle:
      - paste: receipt.json (one per match_id, never merged)
        content: outcome receipt (see receipt_fields)
        rules: [must parse as JSON, every field present, unknown => null]
        budget_lines: 120
    per_batch:
      - issue_header_fields
      - coach summary line
      - the six evidence tables (issue_body.parts[2])
      - pasted_via: python tools/collect_log.py <log>  (one command, output pasted whole)
    local_only:
      desc: yours to keep, never to send
      items:
        - the raw stdout capture (ours.jsonl or whatever you call it) — the collector's input
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
    - receipt.json parses and every field name in receipt_fields is present
    - the collector's own `事件计数：` line reports round= >= rounds in that battle
    - git cat-file -t <base_commit> == commit
    - receipt.coach.moves count == the M in the collector's `本场汇总：移动 M 次` line
    - no null replaced by 0 / "" / "unknown" (result is the sole exception)
    - anything unobtainable is named in the gaps part with a reason, never silently omitted
    - all six evidence tables were run, each present even when it matched zero rows
    - no evidence table was replaced by a verdict
    - the six tables came from `python tools/collect_log.py <log>`, pasted whole, nothing hand-edited
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
        title: 证据表（v17 共 18 张）
        desc: >
          One command, over the local stdout capture, pasted whole. The script already
          aggregates and already caps each section, so the whole output goes in; the all-match
          run of a 10-day, 3-tower, 16-session battle is 250 lines / 7.0 KB (219 table rows).
          Never paste only the first few rows, never replace a table with a verdict, never
          hand-edit the output, and never send a path — a file on your disk is not a
          deliverable. If one table blows past its budget, paste it anyway and say why (gate
          table: the gate WAS open that long).
        produced_by: python tools/collect_log.py <log>
        cap_lines_total: 680   # the script's per-section caps summed; a typical battle prints 250 lines (219 rows)
        tables:
          - id: tower_plan
            sections: [表 1 造塔计划: 20 行]   # 上限写在脚本的 CAPS 里，这里是同一组数
            name: 造塔计划
            tells: why the third tower never went up — mayBuild (weapons) and upgradeReachable (base level) are different doors, and if they take turns being true with nothing built, the block is not money but nobody walking to the site
          - id: ledger
            sections: [表 2a 一进一出: 60 行, 表 2b 想买 vs 买得起: 20 行, 表 2c 钱包台阶: 60 行]   # 上限写在脚本的 CAPS 里，这里是同一组数
            name: 经济台账
            tells: whether frozen gold is an income problem (no sell) or a spending problem (guard reserve blocks the buy) — sell is income, buy is outgo, shopping is "wanted to buy, did we manage"
          - id: task_endings
            sections: [表 3a 结局直方图: 20 行, 表 3b 逐 session: 40 行]   # 上限写在脚本的 CAPS 里，这里是同一组数
            name: 任务结局
            tells: timeout and three-strikes are different diseases; a timed-out task is settled on the best answer ever submitted, so reason=timeout does not mean zero
          - id: judge_verdict
            sections: [表 4a 每次提交: 40 行, 表 4b 判题器回过错的回合: 40 行]   # 上限写在脚本的 CAPS 里，这里是同一组数
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
          - id: gate
            sections: [表 5a 封门逐回合: 160 行, 表 5b 每天开了几回合: 20 行, 表 5c 卡在谁身上: 20 行]   # 上限写在脚本的 CAPS 里，这里是同一组数
            name: 封门
            tells: how many rounds the gate stayed open AND who it was waiting on — the dusk check runs every round from dayRound 55 to 69, a window of exactly 15 rounds, so 15 rows in one day means the gate never sealed at all and the ring had a hole all night
            fields:
              away: "[id, x, y] per role that is neither home nor on the operating cells of the tower it mans tonight"
              stuck: "the subset of away that cannot walk to its post at all — walled off from its gun, so the gate will never seal; absent when empty"
            gotchas:
              - the window is 15 rounds (dayRound 55..69), so 15 rows is the maximum, not a count of seal attempts
          - id: night_debug
            sections: [表 6a 夜间沉默原因计数: 20 行, 表 6b 哪座塔在沉默: 40 行]   # 上限写在脚本的 CAPS 里，这里是同一组数
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
              - 'pairs is pruned away when empty, so the key can be missing entirely — read that as "no rows", not as a broken log'
          - id: enemy_build
            sections: [表 7 对手建造节奏: 12 行]   # 上限写在脚本的 CAPS 里，这里是同一组数
            name: 对手建造节奏
            tells: whether the opponent arms before they fortify — the owner's report was "they build weapons first, we build walls", and this is the only place that claim can be checked against a real match (the receipt field `enemy_seen` has twice come back unobtainable)
            fields:
              enemyTowers: '{"count": n, "kinds": ["gatling", ...]} — the opponent's guns, from `teamEnemy.roles`, which we have always been sent and never logged'
              enemyWall: '{"count": n, "hp": n} — logged since v1'
              enemyBase: '[hp, level] — logged since v1'
            gotchas:
              - 'all three blocks are change-gated (§2 rule 2): a round without `enemyTowers` means UNCHANGED, not "no guns" — read the series with carry-forward'
              - 'the table prints one row per day, at the day''s last round, so read the day-1 row for "did they have three guns on night 1"'
      - id: gaps
        title: 缺口
        content: everything unobtainable, with the reason. A table that printed （0 行） and a thing that did not happen are different claims — say so when you cannot tell them apart.
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
      - "sell events now exist at all (the ledger table used to print （0 行）)"
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
and we do: `/docs/WORKFLOW_REQUEST.md` v13 asks you to (1) keep a raw capture of our stdout,
(2) run one command over it, and (3) paste the batch header fields, one outcome receipt per
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
   endings, the judger's verdicts, the wall gate, and night_debug. The collector already
   aggregates: all six together come to 224 lines / 6.3 KB on a 10-day, 3-tower, 16-session
   battle. One command produces all six (`python tools/collect_log.py <log>`), so there is
   nothing to decide about how much to paste. **This is the part we need most**: you can see the
   result and we cannot, so these tables are the only process data that reaches us. A table
   replaced by a verdict is worth nothing, and "the table printed （0 行）" is not the same
   claim as "it did not happen".
5. **Self-check** (§6) — run it before publishing; it catches missing fields, a wrong
   `base_commit`, count mismatches against our own log, a table gone over budget, and a local
   path left in the body.

**There are no questions to answer.** v9 removed the reverse-questioning of v8 (§7.1's six
mechanism questions and §7.2's three open questions): deliver the rows, and the analysis is
ours to do.

The `coach` block is the `--coach` output in §5; the coach itself
needs no environment variables and no control group from you.

---

## 10. 请求六：v13 落地后新暴露的三处观测缺口

> **这不是新任务，是把六张表补成能回答新问题的样子。**
> 2026-09-14 的 P0 改完了任务反馈环、第 3 塔排序、墙优先与"不把闲人封死在外"。
> 改完之后才发现：**有三件事，改动的成败只能从日志里看出来，而日志现在看不见。**
> 三条都按 `是什么 / 为什么需要 / 建议格式` 写。要不要做、什么时候做你们定；做了就在
> 下一批的 issue 里自然出现，不必额外说明。

### 请求六之一：`tower_plan` 要能区分"钱够但被墙环闸住"和"钱够但没人走到工地"

**是什么.** `tower_plan` 事件（表 1）加一个布尔字段 `holdForRing`：本轮建塔步骤是否被
"墙优先"闸主动跳过（第 1 天、墙线仍有缺口、且已经有一座塔）。

**为什么需要.** 表 1 现在的读法（§7.3）是：`mayBuild` 与 `upgradeReachable` 轮流为真而塔
迟迟不落地 ⇒ **卡点不在钱上，而在"没人走到工地"**。v13 之后这句话在第 1 天**不再成立**：
`may_build_weapon` 只问"钱够不够"，而第 2/3 塔此时是被计划**主动压住**的——墙环没成型，
先建塔等于把当天的走路预算花在塔位与墙线之间的折返上（分析 §3.4，实测旧逻辑 R2 建塔、
全天 3 塔 0 墙）。没有这个字段，下一批的表 1 会把"按设计压住"读成"走路失败"，也就是把
刚修好的墙优先顺序当成新 bug 再修一遍——上一批 5 场 0:3 里，第 3 塔缺席正是靠表 1 归因的，
这条读法一错，归因整条都错。

**建议格式.** `tower_plan` 增加一个与 `mayBuild` 同级的字段：

```text
"mayBuild": true, "holdForRing": true, "towers": 2, "wallGaps": 13, ...
```

（若只想加一个字段，就让 `mayBuild` 输出**实际生效**的判定——但我们更希望两个都在：
"钱够不够"和"计划让不让建"是两件独立的事，合起来才能读。）

### 请求六之二：表 5c 的 `stuck` 要包含"无塔可守"的角色

**是什么.** `wall_gate_open` 的 `stuck` 目前只统计**有塔可守、且走不回炮位**的角色。
请把"无塔可守（空闲）且走不回环内"的角色也算进 `stuck`；`away` 的算法不变。

**为什么需要.** v13 的 P0-3 立的规矩就是：**没有塔可守的角色，家在环内**
（`interior_cells`，也就是 `update_wall_gate` 判定"所有人都进来了"的那一圈）。
而 `wall_gate_open` 的 `stuck` 判定只看该角色的**炮位**（`night_goal`），对空闲角色恒为假——
于是 pk584929 / pk584881 / pk584875 那三场"门 15 回合未封"里，5c 会把空闲角色列进 `away`，
却**永远不列进 `stuck`**，读表的人只能从坐标猜"它到底是被墙挡住了，还是只是在路上"。这正是
表 5 存在的理由（"门卡在谁身上"），而它对 P0-3 关心的那一类角色是失明的：下一批若再出现
整夜不封，我们分不清"空闲角色被封死在外"（P0-3 没修好）和"它只是走得慢"（正常）。

**建议格式.** 字段形状不变（`stuck` 仍是 id 数组），只是集合变大：判定改成
**该角色到它的夜间岗位不可达**，其中"夜间岗位"= 有塔配对的取炮位、没有塔配对的取环内 band。
文档 §7.3 表 5 的 `stuck` 说明相应改成一句：

```text
stuck: away 里根本走不回岗位的那几个——有塔可守的回不了炮位，没塔可守的回不了环内 band。
```

### 请求六之三：提交记录要能看出"判题器的原话有没有进 prompt"

**是什么.** `task_answer_submit` 增加一个字段，记录这次重试的 prompt 里带了几条判题器
原话（反馈条数）；或者，`prompt_sent` 除 `head`（头 300 字符）之外再给一个 `tail`。

**为什么需要.** v13 的 P0-1 把判题器的拒绝原文（`errors[].description`，如
`MissingNamedInput: city`、`键值比对不通过: $/token: 缺少键`）接回了重试 prompt——这是
"任务 0 分"唯一权威的错因，之前一直被解析、被写进 `round.errorDescs`，然后**扔掉**。
接的位置是 prompt **末尾**（§2 的 `prompt_sent.head` 只记头 300 字符），所以那段反馈
**在日志里永远看不到**。结果就是：表 4b 能证明"判题器判错了"，却证明不了"错因有没有
被喂回去"，下一批无法回答唯一重要的问题——**P0-1 到底生效了没有**。而 P1 的下一步
（"新拒绝原文重置 `MAX_WRONG_ANSWERS` 计数、重复原文才放弃"）整个建立在"这条反馈真的
到了 prompt 里"之上；这一格没有，P1 就是在猜。同样的缺口也解释了为什么"反馈环断裂"
能连续几轮修在提交前打转（分析 §3.1）。

**建议格式.** 二选一即可，前者更省地方：

```text
{"event":"task_answer_submit","data":{"session":3,"round":61,"chars":208,
  "rejectionFeedback":2, ...}}          # 本次 prompt 携带的判题器原话条数，0 = 没带
```

```text
{"event":"prompt_sent","data":{"head":"…300 字符…","tail":"…300 字符…"}}
```

---

## 11. v14：P1/P2 落地后，表里多了什么、读法变了什么

> **这一节没有新任务。** §0 那三件事、§7.3 那一条命令，一个字都没变：
> `python tools/collect_log.py <日志文件>`，输出原样贴进 issue。
> 这一节只是把"新出现的列怎么读"写下来——上一批的教训是**把按设计发生的事读成新 bug**，
> 比看不见更贵。

### 11.1 请求六之三已落地，而且是三件里唯一需要动日志的那件

`task_answer_submit` 现在带 `rejectionFeedback`（本次重试 prompt 携带的判题器原话条数），
表 4a 末列已印它。**这一格为 0 就说明反馈环是断的**，那么同一行的"判错"不能算在重试策略头上。
请求六之一（`holdForRing`）与六之二（`stuck` 含无塔角色）**仍未做**，不要从本批的表里
推断它们已经生效。

### 11.2 两个计数器：`rejections` 单调，`wrongAnswers` 会归零

P1-1 之后，"判错几次"这个问题有两个答案，表 3b / 4a 两列都印（详见 §7.3 表 3 上面那段
和表 4a 的例子）。**读法一句话**：`拒绝次数` 是"烧掉了几个答案"，`判错次数` 是"离放弃还有
多远"。`拒绝次数=4 判错次数=0` 是**好消息**（判题器每次都在教新东西），不是"没被拒过"。
`task_ended.reason=wrong_answers` 现在特指**判题器连续重复同一句话**导致的放弃——它变少了
才是 P1-1 生效的证据，而它变成 0 的同时 `拒绝次数` 也没涨，那是反馈环断了（先看 11.1）。

### 11.3 P2 的四条线各自在日志里留了什么

按"能不能从这一批的表里看出它跑没跑"列出来，都是**已经写在 stdout JSONL 里**的字段：

| 哪条线 | 事件 / 字段 | 读它回答什么 |
|---|---|---|
| 第二层墙（P2-1） | `tower_plan.secondLayer`（表 1 末列）、`wall_build.layer` | 外层墙本轮要了几格（`layer:3`）、底下那一圈还差几格（`layer:2`）。**`tower_plan.wallGaps` 现在含外层**，只看总数会把"按设计只买 4 格外层"读成"墙没建完" |
| 宝藏线（P2-2） | `treasure_summon`、`treasure_wait_gold`、`tower_plan.treasureReserve` | 有没有真的献祭过、是否卡在钱上（`gold` vs `reserve`）、为献祭扣住了多少金币。三条都印 0 行 = 这条线一整场没启动 |
| 压制窗口（P2-3） | `boss_suppression` | BOSS 令是**因为**对方基地低血/炮塔扎堆才买的（而不是"有钱就买"）。它出现说明窗口判定生效 |
| 任务收入（P2-4） | `round.taskGoldEarned`、`task_reward` | 任务线累计挣到的**金币**（任务点自己标的 `goldReward`，是上界：判题器按 奖励×通过率 结算而从不说通过率）。它和 `scoreAttr` 三个分量并列，是"任务修复有没有让任务线开始挣钱"唯一看得见的一格 |

### 11.4 一句话总结这一批的表怎么变了

- 表 1：末列 `外层缺口`。
- 表 3b：`判错次数` 前面多了 `拒绝次数`。
- 表 4a：末两列 `拒绝次数`、`带回错因`。
- 表 2、表 4b、表 5、表 6 不变。
- **没有新增小节**，十三个小节的形状和行数上限（§7.3 / §8）都不变（**v15 扩充了表 4，见 §12**）。

---

## 12. 请求七：三处观测缺口（v15，已落地，照旧只交数据）

> **这一节没有新任务。** §0 那三件事、§7.3 那一条命令，一个字都没变：
> `python tools/collect_log.py <日志文件>`，输出原样贴进 issue。
> 这一节说明**多了三张表**（新增表 0，表 4 多了 4c / 4d），以及为什么需要它们。

### 12.1 是什么

2026-09-14(b) 那五场（issue #111-#115），每一场都有 8-13 条 `task_cmd_failed`。它的字段
长这样，五个 issue 里**一模一样**：

```text
{"data": {"exit": 0, "timeout": false}, "event": "task_cmd_failed", "ts": 1789364556125}
```

`exit: 0` / `timeout: false` 是"沙盒把脚本跑完了"。可这个事件叫 **failed**，于是两批分析
都把它读成"命令执行失败、沙盒有问题"。**它不是**：它是"脚本跑完了，但没有打印 `ANSWER:`
行"，也就是——任务线一个答案都没交上去。同一批的 `task_ended` 全是
`reason=timeout, rejections=0, wrongAnswers=0`，正是这件事的后果：**没有答案，就没有拒绝，
P0-1 那条判题器反馈环根本没有东西可作用**。这是比上一批 P0-1 更靠前的一层。

真正让这件事查不下去的是第二个缺口：`cmd_result` 只印字符数。

```text
{"data": {"chars": 2390, "requestRound": 12, "session": 1}, "event": "cmd_result"}
```

一场 14 条命令，条条都是"返回了 2390 / 3081 / 5467 个字符"，**没有一条说返回了什么**。
于是"模型一直在跑 `find`/`cat` 侦察脚本、从没算过答案"这个结论，只能从 `cmd_sent.head`
的脚本内容去倒推，而 `cmd_sent` 里也没有脚本的**输出**。

### 12.2 为什么需要

有两个问题只有这两格能回答，而它们决定了下一批改哪里：

1. **是模型不会算，还是它根本没看到题目？** 脚本 cat 出了任务文件、返回 5467 字符，但
   喂回 prompt 的只有最后两条、每条截 600 字符——任务要求多半在第 600 字符之后。若输出
   里确实有任务全文，那是**上下文窗口**的问题（改 prompt）；若输出里只有报错，那是**沙盒
   路径**的问题（改脚本）。这两条路的修法完全不同，而现在的日志对二者**不可区分**。
2. **同一条命令重试了几次？** `streak` 印的是"连续第几轮没有答案"。1 和 5 是两回事：
   1 是脚本要分两步，5 是死循环。

### 12.3 建议格式之一、之二（**已经在跑了，不用你做任何事**）

`tools/collect_log.py` 从 v15 起自动印这两张表，位置在表 4b 之后：

```text
----- 表 4c · 每条沙盒命令　列：回合 session 字符数 有答案 结果开头（`有答案` 空 = 该日志早于 v15，没有这一格） -----
12	1	2390	false	=== RECON === find: /tmp/selfEvolutionTask/1-fixed-step/task_1_alpha.md
15	1	3081	false	=== Reading Task File === 总 数
18	1	5467	true	ANSWER: 42

----- 表 4d · 没有答案的原因　列：次数 原因（`no_answer_marker` = 沙盒跑完了、脚本没打印 ANSWER 行，不是沙盒故障；`timeout` = 脚本超过 15 秒；`exit_nonzero` = 脚本自己退出码非 0） -----
1 exit_nonzero
2 no_answer_marker
```

**怎么读**（三句话）：

- `有答案=false` 的行，`结果开头` 就是那个脚本**实际打印的第一句**。整列都是
  `=== RECON` / `find` / `ls` ⇒ 侦察死循环，不是沙盒故障。
- `表 4d` 里 `no_answer_marker` 占多数 ⇒ 命令通道没坏，是**脚本从不作答**；这时
  `表 3b` 的 `拒绝次数` 一定是 0，两者对得上才算读对。
- `有答案` 空着 ⇒ 那条日志是 v15 之前跑的。**空不是 false**，不要当成"没有答案"。

### 12.4 之三：分数归属一直躺在日志里，从没印过（新增表 0）

前两处是"日志里没有"，这一处是"日志里有、收集脚本没印"。

`任务书` 第六章：`总积分 = score_1(任务) + score_2(击杀) + score_3(生存)`，
`score_3 = Σ 10×day×存活系数`，**满 550**。回执里只有**总分**（本批是 41 / 34 / 111 /
141 / 47），于是"这 41 分是怎么来的"只能从 `coach_night` 的夜间掉血倒推——倒推得出
"基地第几天倒"，推不出三块各多少，而这三块的修法完全不同：score_1 要修任务线，
score_2 要修夜战火力，score_3 要修**基地别死**。

而这三块**每个 `round` 记录都带着**：

```text
{"data": {"round": 1, "score": 0, "scoreAttr": {"kill": 0, "killThisRound": 0, "residual": -10, "survival": 10}, ...}}
```

`scoreAttr.kill` 是 score_2（累计），`scoreAttr.survival` 是 score_3（累计，所以不用自己
按天加），`residual` 是余项（score_1 加一点归属误差，`abreport.rs` 就是这么读的）。

新增的表 0 每天印一行：

```text
----- 表 0 · 分数归属（每天最后一个回合）　列：第几天 回合 总分 击杀分 survival residual 我方基地 对方基地 -----
1	130	41	11	30	0	0	1500
```

**怎么读**：`survival` 停在哪一天不再涨，基地就是哪一天倒的；`击杀分` 一整场贴着 0，
score_2 就没挣到；`总分` 减 `击杀分` 减 `survival` 就是任务线（score_1）。这一行能让下一批
**不用再倒推**——它也是"改动有没有让某一块真的动起来"唯一直接的一格。

### 12.5 一句话总结这一批的表怎么变了

- 新增 **表 0 · 分数归属**（第几天 回合 总分 击杀分 survival residual 我方基地 对方基地，封顶 12 行）。
- 新增 **表 4c · 每条沙盒命令**（回合 session 字符数 有答案 结果开头，封顶 40 行）。
- 新增 **表 4d · 没有答案的原因**（次数 原因，聚合式，封顶 20 行）。
- 表 1、2、3、4a、4b、5、6 **不变**；收集命令不变。
- 行数预算 560 → **640**（"每天最多 15 行"的表 5a 等上限一个都没动）。

### 12.6 v16 一句话

§13 请求八**只加一张表 7（对手建造节奏，每天 1 行，封顶 12 行）**，行数预算 640 →
**660**。交付方式、收集命令、其余十六张表一个字没改；采集命令照旧
`python tools/collect_log.py <日志文件>`，表 7 会自动多印一段。

---

*本文件由本地 agent 维护，作为向内部 workflow 下发数据交付指令的接口。*

---

## 13. 请求八：对手的建造节奏（v16）

### 13.1 这一节要什么：**重跑一次那条命令就行**，交付方式一个字没变

```bash
python tools/collect_log.py <你的日志文件>
```

这一版**不需要你做任何新动作**：收集脚本会自己多印一段**表 7**（对手建造节奏，
每天最后一个回合一行，封顶 12 行）。

```text
----- 表 7 · 对手建造节奏（每天最后一个回合）　列：第几天 回合 对方塔数 对方塔型 对方墙数 对方基地等级 -----
1	130	2	gatling,railgun	17	1
2	260	3	gatling,railgun,rocket	19	1
```

起因是这一批的 Part B：老板看了比赛，说"**对手优先造武器，我们优先造墙**"。
这条观察的对手那一半，`receipt.enemy_seen`（§3）报过两次"拿不到——对手基地等级/塔数/
墙数在详情页上没有，我方 stdout 也看不见"。**前半句对，后半句不对**：`teamEnemy.roles`
里一直带着对手的所有单位（塔、墙、基地都在），我们的 `round` 记录里也一直写着
`enemyWall` 和 `enemyBase`——缺的只是 `enemyTowers` 那一格。这一版把它补上了，所以
**对手的建塔节奏现在能从我们自己的日志里读出来**，不再依赖那个拿不到的回执字段。

三个块都受 §2 压缩规则 2 约束（**没变的块不重写**），脚本已经按"带值前行"读——
某回合没有 `enemyTowers` 键不是"对手没有塔"，是"和上一回合一样"。

### 13.2 这一格能定什么

拿我们自己的日志 + 规则推"墙优先还是武器优先"，这一批能证明的是：

- 我们的**第 1、2 座塔在 R2** 就起来了，**第一块墙是 R16**——所以"我们优先造墙"
  在**前两座塔**上不成立；
- 真正被"墙优先"压住的是**第 3 座塔**：P0-4 的闸门把它顺延到**第 2 天第 2 回合**
  （本机 day-1 仿真实测 R132），第 1 夜因此是**两门炮打三个角色**，多出来的那个角色
  既没有火力、也没有黄昏归位的岗位（这正是 P1 封门那条的病根）；
- 而把第 3 座塔提到第 1 天，实测的代价是**环上留一个洞**（19/20，
  `ring_ever_complete` 不置位，第 2 天的补墙预算掉回 6 格，也就是 issue #21 那个死法）。

**表 7 能让最后这句从"两难"变成"有答案"**：

- 如果对手第 1 天就有 3 门炮、而且基地比我们活得久 → "武器优先"赢，环上那个洞要另想
  办法补（例如把第 3 座塔挪到不挡西侧那块墙的塔位），下一批就按这个方向改；
- 如果对手第 1 天同样只有 2 门炮 → 这一批"保持墙优先"的结论成立，这条可以结案。

**怎么读**：看**第 1 天那一行**的"对方塔数"和"对方墙数"。塔数 3 = 对手第 1 天就三炮；
塔数 ≤ 1 而墙数 ≥ 10 = 对手也是先墙。基地等级那一列顺带回答"对手是不是先升基地"。

### 13.3 拿不到就写一句

表 7 印出 `（0 行）` 或者"对方塔数"整列是空的，就照 §7.4 写"拿不到 + 为什么"，
**不要填 0**——填 0 和"对手一座塔都没有"长得一模一样，而这两件事的结论正好相反。

---

## 14. 请求九：把分数拆开——我们这 550 分丢在哪，对手那 1000 分从哪来（v17）

### 14.1 这一节要什么

**一半不用你做新动作**：重跑一次那条命令就行，

```bash
python tools/collect_log.py <你的日志文件>
```

脚本会自己多印一段**表 8**（每天最后一个回合一行，封顶 20 行）。字段全部来自我们
自己 `round` 记录里已经有的 `score` / `scoreAttr` / `stationHp` / `enemyStationHp`：

```text
----- 表 8 · 我方分数三块与生存分应得（每天最后一个回合）　列：第几天 回合 总分 scoreAttr.kill scoreAttr.survival scoreAttr.residual 生存分应得 我方基地HP 对方基地HP -----
1	130	15	51	10	-46	10	1080	1500
2	260	21	141	30	-130	30
3	384	21	234	60	-223	60		75
```

**另一半是这一节真正要问的**，见 14.4：对手那 488-1151 分是怎么来的。

### 14.2 表 8 为什么需要

任务书第六章把总分拆成 `score_1 + score_2 + score_3`，`score_3 = Σ 10×day×存活系数`
（满 550）。可这五场里我们**只知道总分**，于是每一批都在用 `coach_night` 的夜间掉血
去倒推「基地哪天倒」，推出「倒在哪天」，推不出「三块各多少」：

- 表 0 的「击杀分」那一列**没说清是哪一方的**。2026-09-14 那五场里它从 49 涨到 577，
  而我方总分只有 31-152——**同一张表上「击杀分」比「总分」还大**，这不是数据错，
  是列名有歧义，读一整批才读明白。表 8 把原始字段名 `scoreAttr.kill` 原样写进列头，
  歧义当场消失（改名的教训：改错一次比不改更贵）。
- **`score_3` 丢了多少，是全场最大的一笔。** 五场里我们都在第 3-6 天被打爆基地，按
  公式 `score_3` 只拿到 60-210，而不是 550。表 8 的「生存分应得」列 = Σ10×d，把
  「应得 vs 实得」摆成相邻两列——**这一条如果成立，就是 550 分里丢了 340-490**，
  但也一直只是「按公式推」，没有一格数据直接确认。

### 14.3 表 8 能定什么

**第一，`score_3` 到底是丢是保。** 「生存分应得」减 `scoreAttr.survival` 如果是
340-490，那么「活到第 10 天」就是全场最大的一块，下一批的力气全该压在防守上；
如果差额很小，那 550 分本来就拿满了，800 分只能从 `score_1`+`score_2` 里出。
**这两条路的改法没有任何重叠。**

**第二，`score_1` 有没有动。** 这一批改了答案的形状（`15` → `{"count":15}`；判题器
点名 `$/token` 时按它点名的键重排）和答案的收割（沙盒没打 `ANSWER:` 行、但打出了
`TOKEN: fc1e78eb2a5a` 时也要交）。预期写在 commit message 里：**表 3a 的
`confirmed_success` 从 5 场 1 次涨到每场 2-3 次**，对应 `scoreAttr.residual` 那一天
应该**往上跳一截**（它包含着 score_1）。表 8 的 `residual` 列是这句话唯一的口径。

### 14.4 拿不到的那一半：对手 488-1151 分是怎么来的

表 7（v16 刚加的那张）在这五场里印出来的是同一个数：**对方塔数 = 0，每一天都是 0**，
对方墙数 8-20。也就是说 **一座塔都不造的对手，积分是我们的一到三十倍**。

塔是 `score_2` 的主要来源之一，所以对手的分大概率不是击杀分，而是下面三条之一：

1. **召唤机器人浪潮**（`BossRobotSummonOrder` 那一套）打我们的基地和角色——那我方
   的应对是**别送人头**（少死角色/少丢塔，`score_3` 也一起保住）；
2. **`score_1` 做得很好**（7 个 session 基本全过）——那应对是**把任务这条线补上**；
3. **打得久**（对手活到第 10 天，`score_3` 满 550）+ 我们死得早——那应对是**拖长**。

**这三条路的改法也不重叠**，而分开它们只需要对手每天的三块分。

**建议格式.** 二选一，都行：

- **A（首选）**：`receipt` 里每场加一块 `score_breakdown`，双方各一行，字段
  `{day, side, total, score1, score2, score3}`——对局详情页上有就抄，没有就写
  「拿不到」；
- **B（退一步）**：在我们自己的日志里，把**对手 stdout 里**的 `round` 记录也收集一遍
  （`teamA_*` 那份，我们手上一直有），表 8 就能并排印双方。

**拿不到就照 §7.4 写「拿不到 + 为什么」，不要填 0。** 对方的塔数**确实是** 0（表 7
已经证了五天），对方的分数不可能是 0——填 0 和「对手一分没得」长得一模一样，而这两
件事的结论正好相反。

---

## 15. 请求十：卡住的那一格到底卡在哪（v17）

### 15.1 这一节要什么：**重跑一次那条命令就行**

```bash
python tools/collect_log.py <你的日志文件>
```

**不需要你做任何新动作。** 表 6b 的「原因」列从这一版起，遇到卡位会自己带上卡点：

```text
----- 表 6b · 哪座塔在沉默　列：回合数 塔id 原因（v17：`controller_stuck` 后面跟 `@x,y/N/M` = 卡在哪一格、几个操作格、旁边有几面可拆的己方墙） -----
11	20020	controller_stuck@30,13/4/0
34	20030	fired
18	20040	cooldown
```

`@x,y` 是角色卡住的那一格，`N` 是它正要走过去的**操作格数量**，`M` 是**紧邻**它的
己方墙数量（也就是 `walk_or_remove_wall` 能拆的那几面）。`fired` / `cooldown` 这些
没有卡点的原因，形状和以前一模一样。

### 15.2 为什么需要

`controller_stuck` 是这一批（issue #121-#125）夜里第二大的沉默原因：**#125 的 20020
卡了 11 回合，#122 卡了 10 回合**，而且 #125 有一个角色**整夜 15 回合钉在同一格
(30,13)**——门开着，炮也满血，就是一整晚没开火。

旧的行里只有 `塔号 / 角色号 / controller_stuck` 三个字段，而夜召真正据以决策的是三件
事：「它站在哪」「它要走到哪几个操作格」「旁边有没有己方的墙能拆」。三件都没有写，
所以"为什么走不动"在日志里**不可读**——这一批读了五场也读不出来。

### 15.3 这一格能定什么

**`@x,y` 在同一个塔下反复出现同一格** = 那里是个**死口袋**（不是"走得慢"）。
**`M = 0` 而 `N > 0`** = 死角里已经**没有墙可拆**，这正是 `walk_or_remove_wall`
唯一答不上来的情形：它的拆墙范围是**紧邻**的己方墙（`chebyshev == 1`），隔一层就够不着。
看到这一行，下一批就能直接决定「把拆墙半径放到 2」值不值——**这是改法，不是猜测**。

**`M > 0` 却还是卡住** = 拆了也不通（拆完 `stands` 仍不可达），那是墙线的形状问题，
和上面的改法完全不同。

**`@x,y` 每回合都在变** = 角色其实在走，只是走得慢（`controller_walking` 那一类），
不用动召回逻辑，该动的是黄昏归位的提前量。

---

## 16. 请求十一：把 score_1 摊到每个 session 上，把城墙的血记成一本账（v18）

### 16.1 这一节要什么

**一半不用你做新动作**：重跑一次那条命令就行，

```bash
python tools/collect_log.py <你的日志文件>
```

脚本会自己多印一段**表 9**（每个 session 一行，封顶 30 行）。字段全部来自我们自己
`round` 记录里已经有的 `scoreAttr` / `taskGoldEarned`，加上 `task_started` /
`task_ended` 的回合号：

```text
----- 表 9 · 逐 session 分数归属　列：session 接取回合 结束回合 结束原因 成功 回合内 residual 变化 回合内 taskGoldEarned 变化 -----
1	11	27	timeout	false	-38	0
2	26	40	timeout	false	-22	0
3	31	46	timeout	false	+9	0
```

**另一半是新字段，一条就够**：`task_started` 请带上这个任务点的
`scoreReward` 与 `goldReward`（`teamOur.playerTasks[]` 里一直有，我们没有落盘）：

```json
{"event":"task_started","data":{"round":12,"timeout":26,"scoreReward":150,"goldReward":30,"head":"请阅读task_1_beijing.md，获取任务信息"}}
```

### 16.2 为什么需要

**第一，`score_1` 是这五场里唯一一笔没被量过的分。** 表 8 的 `residual`（= `score_1`
加归属误差）**是全场累计的**：#130 从 R1 的 -10 走到 D4 的 -123，中间七个 session
哪一个贡献了多少，累计列上完全看不出来。而我们这一批改的正是任务线的两处——
沙盒前的固定预处理（`表 4d` 的 `exit_nonzero`/`no_answer_marker` 一共 22 次）和
答案形状，两处都**只影响某一个 session**。没有表 9，「改了有没有用」只能靠
「表 3a 的 `confirmed_success` 从 0 变成几」去猜，而那一列**本来就是 0**（五场 27 个
session 全部 `success: false`）。

**第二，`task_started.scoreReward` 是一个 session 的天花板。** ch.6 写明
`score_1 = 任务积分奖励 + 5 × 标准回合数 / 实际完成回合`，而"任务积分奖励"我们从没记过。
一个值 300 分的 session 和一个值 20 分的 session 值得投入完全不同的回合数——现在
日志里两者长得一模一样，于是每个 session 都被当成 250 回合的通用任务在跑。

**第三，城墙只记了"丢"没记"补"。** `coach_night` 有 `ourWallLost`，表 6a 有塔的沉默
原因，而**城墙回血一格都没有**。这一批新加了夜间的修墙（炮冷却时操作手用 WallFixer
补身边那面墙，见 §16.4），它每回合最多修 1 面、修一次回满——效果**只体现在"第二天
天亮城墙还剩多少"上**，而那个数现在读不到。

### 16.3 建议格式

**表 9（脚本印）**：按 session 把 `scoreAttr.residual` 和 `taskGoldEarned` 的
**首末差值**摊出来。一个 session 的区间 = `task_started.round` 到 `task_ended.round`，
没有 `task_started` 的用 `task_accept.round`。

**新增字段（一行）**：`task_started.scoreReward` / `task_started.goldReward`，
取自 `teamOur.playerTasks[]` 里该任务点的 `scoreReward`/`goldReward`。

**夜间城墙收支（一行）**：每天夜里（我们自己的 `round` 里 `isDay:false` 的区间）
把 `wall.hp` 的**首值、末值、以及区间内每一次上升之和**印成一行，形如
`第几夜 黄昏HP 天亮HP 修回HP`。`wall.hp` 是 `round` 里 `chg` 带 `wall` 时才有值的块，
按 §2 规则 2 带值前行即可；`sum(上升)` 就是修墙真正补回来的血。

### 16.4 这一批改了什么（不必再报）

- **固定沙盒预处理**（`executeCmd` 开头自动加一段，模型看不到也不用写）：把任务目录下
  `check`/`*.sh` 的 CRLF 去掉并加执行位、导出 UTF-8 locale 与 `PYTHONIOENCODING`。
  对应 `表 4c` 里点名过两次的 `/bin/sh^M: bad interpreter`（#127 r18、r173）
  和 `'ascii' codec can't encode characters in position 33-34`（#127 r39）。
  **预期**：`表 4d` 的 `exit_nonzero` 从每场 2-8 降到 0-2。
- **夜召的死口袋**（`表 6b` 的 `@x,y/N/M`）：`walk_or_remove_wall` 只在"拆这一面就能通"
  时才动手，口袋深两层就一条指令都不发。现在改成**朝炮位拆**、拆完**再走进那个缺口**。
  对应 `#126 20040@27,13/5/1 ×11`（同一角色也是当天 11/11 个黄昏回合 `wall_gate_open`
  点名的那一个）、`#129 20020@32,10/3/3 ×14`。
  **预期**：`表 6b` 里 `controller_stuck` 的同一格连续回合数从 11-14 降到 ≤3，
  多出 `controller_digging` 与 `break_out` 两个事件名。
- **炮冷却时修墙**：塔 `cooldown` 那 33-51 个回合（#126 的 20040 独占 51）操作手原本
  站着不动，现在用 WallFixer 补身边最破的那面己方墙（**同格有机器人时不修**）。
  **预期**：新增 `wall_mend` 事件；配合 §16.3 的城墙收支，`修回HP` 应显著大于 0，
  且 `表 8` 的 `我方基地HP` 每天收盘值抬升。

**拿不到就照 §7.4 写「拿不到 + 为什么」，不要填 0。** 「修了 0 面墙」和「没记这件事」
长得一样，而这两件事的结论正好相反。

---

## 17. 请求十二：天亮清除的机器人不是击杀，首次提交的外形要看得见（v19）

### 17.1 这一节要什么

**一半不用你做新动作**：重跑一次那条命令就行，

```bash
python tools/collect_log.py <你的日志文件>
```

脚本会自己多印一段**表 10**（每天一行，封顶 12 行），字段全部来自我们自己已经在写的
`dawn_clear` 事件（`round`、`day`、`cleared`、`clearedScore`、`scoreDelta`、`score`）：

```text
----- 表 10 · 天亮清除账　列：第几天 回合 清除数 清除的分数 该回合 scoreDelta 当时总分 -----
2	131	31	38	0	112
3	261	24	29	0	158
```

**另一半是表 4a 加一列**，读 `task_answer_submit.shape`，它已经落盘了：

```text
----- 表 4a · 每次提交　列：回合 session 字符数 外形 换过外形 被改写 判错几次 拒绝几次 带回错因 -----
22	1	24	wrapped	false	true	0	0	0
23	1	24	as-is	true	false	0	1	1
```

取值四个：`as-is`（原样交）、`wrapped`（裸值包成单键对象）、`unwrapped`（单键对象
拆成裸值）、`rekeyed`（被搬到判题器点名的那个键下面）。

### 17.2 为什么需要

**第一，`residual` 现在是脏的，而它是唯一一格归因。** `chg` 里的 `scoreAttr` 是
`total - kill - survival`，`kill` 由我们数「上一回合还活着、这一回合不在了」的机器人
得到。任务书 4.7.3：「黑夜结束后，在第二天早上的第一个回合，残余机器人自动清除」——
清除不是击杀，4.7.2 的积分是「机器人**击杀**数 × 机器人积分」。我们连着五批把这件事
数错，差额一晚一次、每晚都在，全部落进 `residual`。表 8 里那五场的
`residual` 是 -106 / -262 / -309 / -409 / -221，**连续五批的分析都把它读成任务线的
亏空**，而任务书 ch.6 的 `score_1 = 任务积分奖励 × 通过率` 不可能为负。
这一批已经在代码里把两者分开（§17.4），但**判题器认不认这部分分，只有把同一个回合的
`scoreDelta` 和 `cleared` 并排才看得见**：`scoreDelta` 约等于 0 就说明不给，
`scoreDelta` 约等于 `clearedScore` 就说明给。这一格定了，`residual` 才能当 `score_1`
用；定不了，下一批还会照着脏数字去改任务线。

**第二，首次提交的外形分不出来。** 这一批改了答案包装规则：以前只有「被拒过一次」
才把裸值包进单键对象，而且只在该值本身**已经是合法 JSON** 时才包。issue #131-#135
五场里，每个 session 的 **第一次**提交（表 4a 第 1 行，7-12 字符）都是沙盒里已经算对的
裸值（如表 4c 的 `TOKEN: fc1e78eb2a5a`），被判题器回 `答案不是合法 JSON`
（表 4b，五场都在 round 23），然后会话的 10-15 回合预算就用完了（表 3b：每个 session
都是 `timeout` + `success:false`）。现在非 JSON 的裸值在**第一次**提交就包好。
`rewritten` 这一列分不清方向——首次包装和拒后拆包都会把它置真——所以要 `shape`。

### 17.3 建议格式

**表 10（脚本印）**：`pick(records, "dawn_clear")`，按 `day` 一行，封顶 12 行。
判题器的 `score` 就是 `round.score`，和 `dawn_clear.score` 同一个数；`scoreDelta` 是
`round.scoreDelta`，同回合的 `round` 记录里一直有。

**表 4a 增列**：直接读 `task_answer_submit.shape`；老日志没有这个键就留空，
不要填 `as-is`——「原样交」和「这条日志早于 v19」的结论正好相反。

行数预算 700 **不变**：`CAPS` 总和 664 → **676**（表 10 十二行；表 4a 只多一列，行数不动）。

### 17.4 这一批改了什么（不必再报）

- **天亮不算击杀**：`round.scoreAttr.kill` 不再把 4.7.3 的清除计入，新增
  `dawn_clear` 事件（每天一行，带 `cleared` / `clearedScore` / `scoreDelta`）。
  **预期**：`表 8` 的 `residual` 从 -100…-400 抬到 0 附近；`表 0` 的
  `总分`/`击杀分`/`residual` 三列重新自洽。**这不改变判题器给的分**，改的是我们
  自己那一列的读数。
- **非 JSON 裸值首次提交就包装**：`task::submittable_answer_for_keys` 在
  「只有一个已知字段 + 答案根本不是合法 JSON」时，第一次提交就包成 `{字段: 值}`；
  本身已是合法 JSON 的值保持原样（首次还是模型自己的外形，重试再换另一种）。
  **预期**：`表 4a` 第 1 行的 `外形` 从 `as-is` 变 `wrapped`，`表 4b` 的
  `答案不是合法 JSON` 从每 session 一次降到 0，`表 3a` 开始出现 `success:true`。
- **夜间修墙真的会修了**：`combat::night_mend_target` 把安全判据从「墙边 2 格内没有
  机器人」（夜里永远为假）改成「操作手没被贴身」，并接到三个新的空转窗口：备用角色
  在**躲进环里之后**、以及炮**可射击但本轮没有值得打的目标**时（表 6a 的
  `no_target_reserved_for_robots`，一场 18-42 回合）。`wall_mend` 事件新增 `duty`
  字段（`cooldown` / `spare`）。
  **预期**：`wall_mend` 从 0 次涨到每夜两位数；配合 §16.3 的城墙收支，`修回HP`
  显著大于 0，`表 8` 的 `我方基地HP` 收盘值抬升。
- **WallFixer 提前到第 1 天买**：`economy::intent_list` 的「环已存在」判据从
  「墙 ≥ 6 面」降到「有墙就行」，数量不变（环没合拢过 2 个、合拢过 4 个）。
  五场里第一个 WallFixer 都买在第 2 天 round 147-154，而第 1 夜已经打完了。
  **预期**：`表 2a` 第 1 天出现 `buy WallFixer`；`coach_night.ourWallLost`
  第 1 夜从 2465-6410 降下来。

**拿不到就照 §7.4 写「拿不到 + 为什么」，不要填 0。** 「判题器不给天亮分」和
「我们没有这一格」长得一样，而这两件事的结论正好相反。

---

## 18. 请求十三：队首买得起却没买——买家那一回合到底停在哪（v20）

### 18.1 是什么

**不需要你做任何新动作**：还是那一条命令

    python tools/collect_log.py <日志文件>

这一版多印一段**表 11（买家回合账）**，数据源是日志里**已经写着的** `shopping`
事件——只是收集脚本以前只从它里面抽了「队首是谁、买不买得起」，其余四格一直扔掉：

| 已经写在日志里的键 | 现在的用途 |
|---|---|
| `buyer` | 这一回合谁负责买（`null` = 今天没有买家） |
| `gold` / `need` / `needNum` / `price` | 表 2b 用的（队首是谁、买不买得起） |
| **`ready`** | `budget.shopping` 的全部条目——**队首之外还有谁买得起**，从没印过 |
| **`buyerShopDist`** | 买家离商店几回合——从没印过 |
| `needs` | `budget.intent` 的全部条目——从没印过 |
| `reserve` / `deadline` | 塔基金与截止回合——从没印过 |

表 11 还要从 `round` 事件里取两格**同一回合**的读数与它对齐（回合号已经是 join key）：

* `roles[].pack` —— 买家背包里装了几件，用来算 `free_slots`；
* `cmd` 里那一回合买家实际收到什么命令（`buy` / `move` / `mine` / `use` / 空）。

### 18.2 为什么需要

这一批的十场里，**表 2a 一笔基地升级券都没有**，而表 2c 显示金币峰值到过
130-155（升级券 100，任务书 4.6.3）。这一批已经修掉了「基地券永远排在武器券
后面」这个结构性问题（§18.4），但**修完能不能落地，取决于买家会不会真的去花这笔钱**，
而这件事现在读不出来：

* issue #170 有 **30 个回合**队首是 `WeaponUpgradeVoucher1`、`affordable=true`
  （金币 ≥ 100），整场**一笔都没买**；
* 同一场第 142 回合买家倒是买了 6 个 `WallFixer`——说明买家**能**走到商店，
  只是那些回合没去；
* `buyer_flow`（`CoreGeek/src/brain/day.rs`）在四种情况下返回 `None` 而不买：
  ① 没走到第 7 步（被挖矿/卖矿/建墙/封门等更高优先级的步骤占了这一回合）；
  ② `should_sell` 为真（背包里矿石 ≥ 4，先去卖）；
  ③ `committed` 为真（黄昏归位已触发，不去远处）；
  ④ 站到商店了但 `free_slots == 0`（背包满，`num = min(need.num, free_slots, affordable) = 0`）。
  **这四种在现有表里印出来是同一个样子**：队首买得起，什么也没发生。

### 18.3 建议格式

聚合式，**恒定二十行**（和表 2b 同一节，不占新预算）。一行一个「停滞原因」，
列：回合数 原因 最后一次的回合 当时的金币 买家离店距离：

```text
表 11 · 买家停滞账　列：回合数 原因 最后回合 当时金币 离店距离
12	no_buyer	                    40	146	-1
9	buyer_never_reached_step7	    51	96	7
6	preempted_by_should_sell	    38	130	4
3	backpack_full_at_shop	        37	130	0
```

`原因` 的取值固定为这五个，别自由发挥（自由文本会被读成五类之外还有别的）：

| 原因 | 判据 |
|---|---|
| `no_buyer` | `shopping.buyer` 是 `null` |
| `buyer_never_reached_step7` | 有买家、`ready` 非空，但 `round.cmds` 里没有买家的任何命令 |
| `preempted_by_should_sell` | 买家的命令是 `sell`（或 `use` 的 `Voucher` 之外的东西） |
| `buyer_walking` | 买家的命令是 `move` 且 `buyerShopDist > 0` |
| `backpack_full_at_shop` | `buyerShopDist == 0` 且买家 `pack` 件数 = `backPackCapability` |

### 18.4 这一批改了什么（不必再报）

- **基地升级券不再被武器券抢走**（`economy::intent_list`）：两张券都是 100 金币，
  而 `shopping_list` 按 `(priority, Reverse(value))` 贪心分配，武器券是 priority 0、
  基地券是 priority 1——于是**只要金币只够一张，基地券永远被跳过**。十场里十次
  如此（表 2a），九场基地倒下。现在两张券同档（priority 0），由 `survival_value`
  决定：基地券按 `effective_hp` 记 2250 有效 HP / 100 金币，武器券 1500——这段
  排名本来就是为这个比较写的，只是被 priority 盖住了。
  **预期**：`表 2a` 出现 `buy StationUpgradeVoucher1`；`表 0`/`表 8` 的
  **`我方基地HP` 上限从 1500 抬到 3000**。判据不在「买没买」而在
  **`我方基地HP` 有没有超过 1500**——超过就说明券真的用上了。
- **基地等级成为一列**（`表 0` / `表 8` 新增「我方基地等级」= `round.stationLvl`）：
  这一列从 1 变 2 是升级券落地的**直接**证据，不用再从掉血速度倒推。
  金币是共用的，所以这一列也顺带告诉你「钱花在基地还是炮上」。
- **收集脚本的回填 bug**（`tools/collect_log.py`）：`stationHp` / `stationLvl` /
  `enemyStationHp` 走的是 `chg` **增量**机制（`brain::mod::respond` 的 `gated`
  循环只在 `base` 块**变动那一回合**才写它），而收集脚本用 `block.get(...)` 直接读，
  于是「这一回合基地没掉血」被读成「基地血量未知」。表 0 / 表 8 的「我方基地」列
  在十份报告里**九份是空的**，而这一列是 `score_3`（满 550）唯一的直接证据。
  现在按回合回填（`carried()`）。**这是读数修复，不改任何行为。**

---

## §19 请求十四（v21）：买家出门账

> **v21 只加一节：§19 请求十四**，别的什么都没动——交付方式、**收集命令一个字没变**，
> 你那边**不需要做任何新动作**。行数预算 `CAPS` 总和 **676 不变**：§19 要的表 11 是
> 聚合式、恒定二十行，与表 2b 共用同一份预算（表 2b 只印队首需求，看不出「买得起却
> 没买」卡在哪一步，这一节就是把它拆开）。另外这一版把**第 N 天商店逐回合**的 drill
> 加了三列（末三列，只在 `--day N` 时印，不进 660/676 预算）。
> 起因是 2026-09-15 那五场（issue #156-#160），**五个对手、五场 0:3**，而这一批的
> 根因**已经由代码里的改动定住了**（§19.4），§19 要的是**验收它的那一格**。

### 19.1 是什么

表 11 · 买家出门账（聚合，一行 = 一种「决定 + 需求 + 买得起否」）：

```text
表 11 · 买家出门账　列：回合数 出门决策 买得起否 需求
```

### 19.2 为什么需要

v20 §18 问的是「买得起却一笔都没买，是没走到第 7 步、背包满了、还是被 `should_sell`
顶掉了」——**答案在 #156-#160 这五场里已经拿到了，而且三条都不是**：

- 买家的**背包没满**（五场里四场整场没有一次 `sell`……**这一条本身就是线索**）；
- 买家**走得到商店**（#159 在 r26 用 130 金币买下了一张武器券，表 2a 有据）；
- 卡住它的是**时间**：买家同时是**炮位操作手**（`preposition_round` 让它在
  黄昏前必须回到炮位），而商店是**地图上最远的一个 errand**。旧的 `buyer_flow`
  只要购物清单非空就出门，**从不比较「这一趟来回要多少回合」和「离黄昏还剩多少回合」**，
  于是走到一半被 `day.rs` step 4 的归位锁**原地掉头**——金币一个没花，回合全烧在路上。
- 复现（`tests/day1_sim.rs`，pk590730 的形状：第 2 天第 23 回合金币跳到 130，
  环上有夜里被打掉的缺口）：修之前买家走到 (26,22)、离柜台两格处掉头，
  当天结束 144 金币、**一次 `buy` 都没有**；修之后第 34 回合站上柜台格买下 100 金币的那件。
- **还有一半**：买家就是那个「专职经济工人」，它的背包**按设计就该装着可卖的矿**，
  所以 `should_sell` 大半天为真，step 7（购物）被 step 9（卖矿）顶掉；唯一不被顶掉的
  那一回合，是它刚卖完、人正站在**小贩**柜台前——离商店最远的角落。

这一批把上面两条一起改了（§19.4）。**但「改对了没有」只能靠日志验收**，而现在的表
里没有一格能区分「出门了」和「没出门」：表 2b 只说买得起，表 2c 只说钱在涨。
出问题的时候（下一批若还有），要能一眼看出是 `no_time`、`pack_full` 还是 `sale_first`
——三者的修法完全不同。

### 19.3 建议格式

数据源就是**已经在日志里的** `shopping` 事件（`event == "shopping"`，每回合一条），
v21 给它加了三个键，收集脚本直接读：

| 键 | 含义 |
|---|---|
| `shopTrip` | 这一回合买家的出门决定：`walk`（出门/已到柜台）、`no_time`（当天来回走不完）、`pack_full`（背包没格）、`sale_first`（有矿要先卖，且这趟不值得走）、`nothing_affordable`（清单为空） |
| `trip` | 从买家当前位置出发，往返商店需要的回合数（出门 + 1 回合在柜台 + 走回炮位） |
| `lockRound` | 它必须回到炮位的那一回合（`preposition_round`，没有炮位时为 `DUSK_ROUND`） |

聚合方式与表 2b 相同（`counted(...)`，按整行去重计数），列序：

```text
表 11 · 买家出门账　列：回合数 出门决策 买得起否 需求
```

**v20 的「表 11 · 买家回合账」如果已经做了，就用这一节的定义替掉**：v20 那三格
（`free_slots` / 有没有走到第 7 步 / 是不是被 `should_sell` 顶掉）现在分别是
`pack_full` / `walk` 对 `sale_first` 的区别，而且新增的 `no_time` 才是这五场的答案。

### 19.4 这一批改了什么（不必再报）

- **购物是一趟有预算的差事**（`day.rs`）：`shop_errand_fits` = 「出门 + 1 回合在柜台
  + 走回炮位」能在 `DUSK_ROUND` 之前走完。`buyer_flow` 只在它成立时才起步——走不完
  的一趟**不再出发**，角色留在当天的工作上，而不是烧十几个回合被掉头。
- **起步的那一趟不许被归位锁打断**（`day.rs` step 4）：`shop_errand_fits` 成立时
  归位锁让路。这两处用的是**同一个判据**，所以不会互相打架：允许起步的一趟一定
  走得完（走出去只会让往返变短，到柜台时仍然成立），不允许起步的一趟根本到不了这里。
  到岗时间的上限是**黄昏**（不是 `preposition_round` 的三回合余量），夜里那条
  无条件召回仍然兜底。
- **卖矿不再顶掉购物**（`day.rs` step 7）：`budget.shopping` 非空（= 手上的金币够付）
  且这一趟走得完时，购物优先于卖矿。旧注释担心的「带着没卖的矿去商店，商店不收矿、
  白跑一趟」在**买得起**的前提下不成立——金币已经在手，矿可以下次再卖。
- **验收判据**：`表 11` 里 `walk` 应当出现在金币到位的那些回合；`表 2a` 应当出现
  `buy StationUpgradeVoucher1`；`表 0`/`表 8` 的「我方基地等级」应当从 1 变 2。
  只买得起一张券时（金币 100-155，表 2c）应当是**基地券**，不是武器券。
