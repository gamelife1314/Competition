# 对战失利分析 2026-09-14（issues #42 + #41）

> 分析日期：2026-09-14
> 分析基线：`main` / `6fd0882`（5 场对战全部使用该版本）
> 数据来源：issue #42（对战数据 2026-09-14 5负）、issue #41（失利原因人工总结 4 点）
> 证据口径：issue #42 的六张表由 `tools/collect_log.py` 从对战日志提取；#42 第四部分明确声明 `receipt.tasks_ours[].score` 与 `receipt.enemy_seen` 拿不到，因此逐场复盘的得分拆解只能到"总分"与"六张表"，不伪造未拿到的逐 session 分。

---

## 0. 结论先行

| 序号 | 主因（对应 issue #41） | 一句话根因 | 优先级 |
|---|---|---|---|
| 1 | 任务始终做不对 | 判题器拒绝原文 `error_descriptions` 已解析、已入日志，却**从未喂回重试 prompt**；LLM 在完全没有反馈的情况下盲改答案 | **P0** |
| 2 | 第 3 座塔第一天建不起来 | `may_build_weapon` 用"潜在财富"`liquid_gold` 判升级是否可达，矿石躺在背包里就永远"可达"，fallback 永不触发 | **P0** |
| 3 | 建墙把人挡在外面进不来 | `wall_would_trap` / `roles_can_reach` 跳过"无塔可守"的空闲角色；白天撤退又不会拆己墙，于是空闲角色被墙环封死在外面 | **P0** |
| 4 | 建墙浪费太多回合在移动上 | 第二天塔（railgun）在墙环还没动工时就建（实测 R2），工人在墙线/塔位之间来回折返 | **P0** |
| 5 | 优先升级武器（矛） | 现有 `intent_list` 已把武器升级券列最优先，但被第 2 条的"先升级后三塔"排序堵死：三塔不建、100 金升级也永远不来 | P0-2 联动 |

**一句话总纲**：5 场 0:3 不是某一处代码"坏了"，而是**任务反馈环断裂**（0 任务分）与**经济/建造的"潜在财富"死锁**（第 3 塔缺席）两个结构性问题的叠加，再被**墙环把空闲角色封死在外**放大成基地被拆。三个问题在耦合环路上相互放大，任何一个单点补丁都救不回来——这也是"修复过多次、反复复发"的元根因。

---

## 1. 逐场复盘（5 场，base commit 6fd0882）

### 1.1 pk584883 vs 汪汪队开大会　0:3　我方 77 : 对手 857

- **塔**：`tower_plan` 最终到达 3 塔（102 个白天回合停在 `towers=3`），但第 3 塔在第 2 天才出现（前 178 回合只有 2 塔）。首夜只有 2 门炮。
- **门/墙**：第 1、2 天黄昏门都成功封上（`wall_gate_seal`），表 5c 无卡死角色——这一场没有"挡人"问题。
- **任务**：7 个 session **全部 timeout**；1 次提交被判"答案不是合法 JSON"（表 4b），其余 0 提交（表 4a 几乎为空）。
- **定性**：这场是 5 场里"最接近"的一场（3 塔建成、门封好），输在**任务 0 分 + 首夜少一门炮**。对手 857 分里大头是任务分，我方 77 分几乎全是击杀/生存残值。

### 1.2 pk584881 vs 泥头车　0:3　我方 71 : 对手 807

- **塔**：`tower_plan` 全程只有 2 塔（`towers=2` 贯穿全天，从未到 3）。
- **门/墙**：第 2 天黄昏 `wall_gate_open` 连续 15 回合（= 整个黄昏窗口），**门从未封上**；表 5c 三角色全部卡在外面：`20011` 23 回合、`20010` 15 回合、`20012` 15 回合，最后位置 (34,9)/(34,7)/(34,6)——全在墙环外侧。
- **任务**：6 个 session 全部 timeout，0 提交（表 4a 空）。
- **定性**：典型的三因叠加——**2 塔 + 角色被封死在外 + 任务 0 分**。

### 1.3 pk584875 vs 谢谢　0:3　我方 126 : 对手 980

- **塔**：全程 2 塔。
- **门/墙**：第 2 天门开 9 回合、第 3 天**门开 15 回合未封**；表 5c `20010` 17 回合、`20011` 14 回合卡在外；表 5a 第 3 天 `20010` 从 (27,8) 走到 (23,13) 再折返——在墙外空转。
- **任务**：10 个 session，1 个 `wrong_answers`（3 次提交，判题器回"键值比对不通过: $/types: 缺少键"，表 4b），其余 timeout。出现了 `task_answer_sentinel`（哨兵答案被拦下，说明 LLM 在跑但答不对）。
- **定性**：对手 980 分是全批最高；我方唯一"像样"的 126 分仍缺任务分。**门封不上是角色被锁在外的直接后果**。

### 1.4 pk584874 vs Engrave0213　0:3　我方 67 : 对手 329

- **塔**：先 2 塔后 3 塔（96 个白天回合 `towers=3`），第 3 塔同样第 2 天才到。
- **门/墙**：第 2 天 `20011` 卡 6 回合（表 5c），门在第 2 天 `wall_gate_open` 6 回合后封上。
- **任务**：6 个 session，1 个 `wrong_answers`（提交被判"键值比对不通过: $: 值不符"），其余 timeout。
- **定性**：这场对手只有 329 分（是 5 个对手里最弱/最不进攻的），我方仍 67:329——**任务 0 分是最大单项差距**（对手 329 里任务分占大头）。

### 1.5 pk584929 vs 一千万以内最好的队　0:3　我方 13 : 对手 213

- **塔**：全程 2 塔（teamA 侧，`towers=2` 始终，从未 3）。
- **门/墙**：**第 1 天门就开了 15 回合未封**——表 5a 第 1 天 `10011` 停在 (13,19) 一动不动 15 回合，表 5c `10011` 卡 15 回合。这是"挡人"最纯粹的一例：第一天就有角色进不了墙环。
- **任务**：3 个 session 全部 timeout，0 提交。
- **定性**：第一天就发生了"空闲角色被封死在外"，门整夜敞着；基地几乎白给（13 分）。

### 1.6 复盘小结

五场有共同的失分结构：

1. **任务分（score1）五场全部为 0**——判题器原话反复给出"缺哪个键"（`$/token`、`$/types`、`$`），但系统把这条唯一权威的反馈**扔掉**了（见 §3.4）。
2. **第 3 塔（score2 放大器）在 3/5 场缺席**（pk584881/875/929），即便建成的两场也是第 2 天才到——首夜永远只有 2 门炮。
3. **门封不上（score3 直接原因）在 3/5 场发生**（pk584881/875/929），根子是空闲角色被墙环封死在外。

---

## 2. 四个失分主因（逐条回应 issue #41）

### 2.1 "建墙浪费太多回合在移动上，把人挡住进不来"

**这是两件事，一件是折返浪费，一件是封死。**

- **折返浪费**：`day1_sim` 实测（本仓库回归测试自带 harness）当前第二天塔 `railgun` 在 **R2** 就建成——在墙环一砖未砌时。工人在"塔位 ↔ 石矿 ↔ 墙线"之间来回折返，这正是"浪费太多回合在移动上"的量化形态。
- **封死在外**：`wall_would_trap`（`day.rs:1371`）与 `roles_can_reach`（`day.rs:1354`）只检查**有塔可守**的角色（`night_goal` 返回 `None` 的空闲角色被 `continue` 跳过）。2 塔 + 3 人时，那 1 个空闲角色的"家"是墙环内侧，但没有任何代码保护它不被封在外。`fallback_toward_station`（`day.rs:395`）与 `retreat_inside`（`day.rs:1318`）只会 `walk_toward`，**不会像夜间的 `walk_or_remove_wall` 那样拆己墙**。于是：空闲角色在外采矿 → 墙环绕过它封口 → 它永远进不来 → 门因"还有人没归队"永远封不上（`update_wall_gate`）→ 整夜敞着洞。pk584929 第 1 天 15 回合、pk584881/875 第 2/3 天 15 回合的 `wall_gate_open` 就是这个链条。

### 2.2 "第 3 座塔第一天始终建立不起来"

根因在 `economy.rs::may_build_weapon`（`economy.rs:641`）的四道门，核心是**"先升级后三塔"的排序 + 用潜在财富判可达**：

```
towers<2            → gold≥25 可建（D1 花掉 50 金）
towers=2 且无 L2     → 需 gold≥125（25 建塔 + 100 升级券）…… 门 A
                      或 third_tower_fallback：gold≥25 且 in_day≥35 且 !upgrade_reachable …… 门 B
towers=2 且有 L2     → gold≥50 …… 门 C
```

- **门 A 永远够不着**：收入冻结，金币从未到 125（钱包台阶表 2c：75→50→25→5，全天在 5-25）。
- **门 B 自相矛盾**：`upgrade_reachable`（`economy.rs:676`）用 `liquid_gold`（`economy.rs:516`，现金 + **背包里可卖矿石估值**）判"升级可达"。只要工人背包里躺着 iron/copper（采石途中顺手捡的），`liquid_gold` 就 ≥100，"升级可达"恒真，fallback 永不触发，第 3 塔永不建。这是 561640b 只剔除石头、**没剔除 iron/copper** 后剩下的另一半死锁。
- **实证**：`day1_sim` 里工人只采石头（石头不计入 `liquid_gold`），所以 fallback 正常触发、第 3 塔 R40 建成；**真实地图里工人会捡 iron/copper**，`upgrade_reachable` 恒真，于是 pk584881/875/929 全程 2 塔——这正是"单元测试全绿、真实对局仍失败"的裂缝所在。

### 2.3 "优先升级武器（先备好最尖的矛，进攻是最好的防御）"

- 现状 `intent_list`（`economy.rs:190`）**已经**把 `WeaponUpgradeVoucher1` 列为 priority 0（最优先），方向是对的。
- 但它被 2.2 的"先升级后三塔"排序反过来咬死：为了攒 100 金升级券，第 3 塔被扣住；而收入又因经济冻结到不了 100 金——**矛没磨出来，第三门炮也没架起来**，两头落空。
- 正确排序（owner 的意图 + Improve.kimi.md §4.2 对照组）：**D1 先把三塔架满（25×3=75 金），再用后续 collect→sell→buy 的收入买 100 金升级券**。对手就是 R1–R4 零储备三塔起手（#20/#9/#15 实测），把升级留给 D2+ 收入。

### 2.4 "任务始终做不对，得分远远落后对手"

- 判题器的拒绝原文 `errors[].description`（如 `MissingNamedInput: city`、`键值比对不通过: $/token: 缺少键`）**已**在 `model.rs:343` 解析、**已**在 `mod.rs:322` 写入 `errorDescs` 日志——但**从未**进入 `task.rs::build_prompt`。
- `build_prompt`（`task.rs:340`）重试时只注入 `schema_gaps`/`schema_extras`，这两个来自 `expected_fields`（从占位描述猜字段）或 `discovered_fields`（靠 LLM 自觉打印 `FIELDS:`）——**都不是权威**。真正点名"缺哪个键"的拒绝原文被扔在日志里没人看。
- 对手 #10 的成功路径就是"读拒绝原文 → 按 `MissingNamedInput` 补字段 → 4 次重试成功"；我方每次重试都拿不到这条反馈，只能盲改，`MAX_WRONG_ANSWERS=3` 又在信息最丰富的前三次后放弃。
- 表 4b 的三种错误码（"答案不是合法 JSON"、"`$/token` 缺少键"、"`$/types` 缺少键"、"`$` 值不符"）全部是 schema/格式问题，不是 LLM 算不出——**是反馈环断裂，不是模型不够聪明**。

---

## 3. 根因定位（到文件/函数，不止症状）

### 3.1 任务反馈环断裂（score1 = 0 的根因）

- **文件**：`CoreGeek/src/brain/task.rs`、`CoreGeek/src/state.rs`、`CoreGeek/src/model.rs`。
- **机制**：`model.rs:343` 把 `errors[].description` 收进 `Turn::error_descriptions`（这一步已修）；`state.rs::absorb_llm_and_cmd`（`state.rs:536`）在提交被判错时只做 `wrong_answers += 1`、`finish_task`，**不把 description 存进 session**；`task.rs::build_prompt`（`task.rs:340`）因此没有任何拒绝原文可注入。`discovered_fields`/`expected_fields` 只是"猜"，权威反馈被切断在最后一环。
- **为什么反复修不好**：前几次修复（哨兵过滤、FIELDS 回显、submit-as-accumulating、答案翻转）都在**提交前**打转，从没把"判题器说了什么"这条信息接回大脑。链路只剩最后一环，也是信息最全的一环。

### 3.2 第 3 塔死锁（score2/score3 的根因）

- **文件**：`CoreGeek/src/brain/economy.rs`（`may_build_weapon`、`third_tower_fallback`、`upgrade_reachable`、`liquid_gold`）。
- **机制**：见 §2.2。核心是 `upgrade_reachable` 用 `liquid_gold`（潜在财富）而非 `turn.gold`（现金）判升级可达；矿石在背包里 → 永远"可达" → fallback 永不触发 → 第 3 塔永不建。这又是 `WEAPON_UPGRADE_RESERVE`（`economy.rs:38`）"为升级扣住 25 金"的连锁：扣住的 25 金既不够 100 金升级，又挡住了 25 金的第 3 塔。
- **为什么反复修不好**：#9 加 fallback、#15 放宽到 125 金、P0-4 加 GUARD——每次都在**阈值**上打转，没人动"升级 vs 三塔"的**排序**本身。

### 3.3 空闲角色被封死（门封不上的根因）

- **文件**：`CoreGeek/src/brain/day.rs`（`wall_would_trap` `day.rs:1371`、`roles_can_reach` `day.rs:1354`、`fallback_toward_station` `day.rs:395`、`retreat_inside` `day.rs:1318`）。
- **机制**：见 §2.1。`night_goal`（`day.rs:1342`）对无塔可守的角色返回 `None`，两个安全函数都 `continue` 跳过；白天撤退只用 `walk_toward`，没有夜间 `walk_or_remove_wall`（`mod.rs:523`）的"拆己墙逃生"兜底。
- **为什么反复修不好**：封门的判定始终以"塔可达"为唯一标准，而"闲人也要回家"这个约束从未进入 `wall_would_trap` 的可达性检查；夜间有拆墙兜底、白天没有，是 #12/#13/#14 修墙环后遗留的另一半。

### 3.4 墙优先的折返浪费

- **文件**：`CoreGeek/src/brain/day.rs`（`worker_day` 第 6 步建塔分支，`day.rs:685`）。
- **机制**：`worker_day` 里"砌墙"（第 5 步）在"建塔"（第 6 步）之前，但第 6 步不判断墙线是否仍在进行：第 1 天工人手里暂时没石头的那一回合就冲去把 `railgun` 建了（`day1_sim` 实测 R2），随后又走回墙线。第 2、3 塔本应等墙环成型再建。

---

## 4. 改进方案（P0 / P1 / P2，含精确代码改动与预期得分）

> 计分基线（任务书第 6 章）：`score1` 任务分 = 任务奖励 + 5×标准回合/实际回合（部分完成 = 奖励×通过率）；`score2` 击杀分 = 小/中/大/BOSS 1/2/4/10；`score3` 生存分 = Σ10×day，满 10 天 550 分。

### P0-1（任务反馈环）：把判题器原话喂回重试 prompt

- **改动**：
  1. `state.rs`：`TaskSession` 增 `rejection_feedback: Vec<String>`（去重）。
  2. `state.rs::absorb_llm_and_cmd`：提交被判错（`errorCode==2` 且 stage=WaitingSubmit）时，把 `turn.error_descriptions` 中对应 code==2 的非空原文存入 `rejection_feedback`。
  3. `task.rs::build_prompt`：末尾追加"判题器对你已提交答案的原话反馈：…"，逐条原文注入。
- **预期得分**：`score1` 从 0 解锁。对手单任务 48–91 分、两任务 139–240 分；即使只做到"按缺键补字段"的部分通过率，每场也能拿回 50–150 分（表 4b 的三类错误全是"缺键/格式"，正是这条反馈能直接修的类型）。
- **风险**：极低（纯增量；description 为空时退化为现状）。
- **冲突**：与硬约束无冲突；`build_prompt` 的 `/tmp/selfEvolutionTask/` 提示、探索输出/元描述/哨兵过滤全部保留。

### P0-2（第 3 塔当天建成）：删除"升级先于三塔"排序

- **改动**：`economy.rs::may_build_weapon` 改为 `towers.len() < 3 && turn.gold >= WEAPON_BUILD_COST`；删除 `third_tower_fallback` 与 `WEAPON_UPGRADE_RESERVE`（升级储备职责交回 `day.rs::tower_build_reserve` 的 1-2 塔金币储备 + `intent_list` 的升级券 priority 0，二者不动）。
- **预期得分**：首夜火力 +50%（3 门炮 vs 2 门），直接抬高 `score2` 清怪分、压低磨墙时间 → `score3` 生存分多撑 1–2 天（每多一天 +10×day）。
- **风险**：D1 金归零（与对手零储备三塔一致）；D1 黄昏靠首批卖矿补药（`READINESS_LEAD` 逻辑已存在）。
- **冲突**：与 issue #3 / AUDIT_PLAN P1-3 的"两塔先升级"直接冲突，按硬约束"三塔上限 + gatling→railgun→rocket + 禁覆盖 + 保留金币储备"执行意图。

### P0-3（不把闲人封死在外）

- **改动**：
  1. `day.rs::wall_would_trap`、`day.rs::roles_can_reach`：无塔可守的角色以 `interior_cells` 为"家"，纳入可达性检查（不再 `continue` 跳过）。
  2. `day.rs::fallback_toward_station`、`day.rs::retreat_inside`：改用 `walk_or_remove_wall`，被封死时拆一堵己墙回家（与夜间召回同一逃生兜底）。
- **预期得分**：`score3`——门能封上、角色不被白送（pk584929 第 1 天 13 分、pk584881/875 门 15 回合敞开的失血直接止住）。
- **风险**：`walk_or_remove_wall` 只在 `walk_toward` 确认无路时拆墙，不会误拆；墙环已由 `wall_would_trap` 兜底。
- **冲突**：无；"夜间召回/预就位"红线不动。

### P0-4（墙优先：环在建时不折返建塔）

- **改动**：`day.rs::worker_day` 第 6 步建塔分支加闸：第 1 天、已有 ≥1 塔、`wall_gaps` 仍非空（= `shared_wall_duty`）时，跳过第 2、3 塔，等墙环成型再建（首塔 `gatling` 豁免）。
- **预期得分**：消除"塔位↔墙线"折返，墙环提前闭合（`day1_sim` 里 `railgun` 从 R2 推迟到环成后），间接抬 `score3`；且保证三塔仍当天建成（环成 → railgun → rocket）。
- **风险**：极端"石头不可达"地图下第 2/3 塔顺延到 D2（该图整天本就无墙可砌，属可接受权衡）。
- **冲突**：硬约束"Day 1 墙优先于武器（环先于第二塔）"即本条目标。

### P1（短期，本轮文档先行、下批实施）

- **信息驱动重试**：`MAX_WRONG_ANSWERS` 改为"新拒绝原文重置计数、重复原文才放弃"，对齐对手 4 次重试路径（依赖 P0-1 已落地）。
- **沙盒 schema 发现**：`SCHEMA:` 标记 + `discovered_schema` 驱动校验与拆包。
- **SOP 两段式**：explore+answer 模板对，压缩第二任务到 2–3 回合。

### P2（中期）

- 承伤扇面第二层墙、宝藏线复活、召唤令压制窗口、得分归因看板（`scoreAttr` 三项 + `task_gold_earned`）。

---

## 5. 与硬性回归约束的符合性声明

- **墙优先（环先于第二塔）+ 最小走动 + 不把人封死**：P0-4 + P0-3 正是这条的实现；墙环仍由石头免费砌成，不花金币。
- **夜间不采矿、闲人先治疗再入环避难**：不动。
- **夜间召回 / 预就位**：不动。
- **建造顺序 gatling→railgun→rocket、三塔上限、禁覆盖**：`tower_gaps` 的顺序与 `validate.rs` 的防覆盖/上限全保留。
- **`cmdRounds=0` 不提前结束任务**：P0-1 是"接取后"的反馈注入，不是早退；`retire_dead_task_point` 语义不动。
- **`build_prompt` 保留 `/tmp/selfEvolutionTask/` 提示、不提交探索输出/元描述/哨兵**：P0-1 只追加反馈，不改提示框架；三重过滤（`is_meta_answer`/`is_failure_answer`/非标记不提取）全保留。
- **金币储备、collect→sell→buy 循环、批量交易、背包容量校验**：P0-2 保留 `tower_build_reserve`（1-2 塔储备）与 `shopping_list` 的储备地板；经济循环、批量、背包校验不动。

---

*本文为分析，代码改动随后的 commit 单独提交。*
