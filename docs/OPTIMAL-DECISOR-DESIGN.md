# 最优决策器设计文档

## 目标

将三个决策维度从"规则驱动"升级为"收益驱动"，使每个回合的决策最大化预期收益。

## 维度一：采矿 ROI 优化

### 现状

`economy.rs:1042` `choose_mine` 和 `economy.rs:990` `choose_sellable_mine` 使用字典序元组：
```
(trip_rounds, Reverse(price), x, y)
```
距离严格主导。5格近铁矿(价8)永远胜过15格远铜矿(价12)，即使后者的 gold/round 更高。

### 设计

替换为 **gold_per_round** 比率排序：

```rust
fn mine_roi(price: i64, trip_rounds: i32, backpack: i64) -> f64 {
    if trip_rounds <= 0 { return 0.0; }
    (price as f64 * backpack as f64) / trip_rounds as f64
}
```

选择器改为按 ROI 降序，平局时取距离更近的：

```rust
options.into_iter().max_by(|(pos_a, ore_a), (pos_b, ore_b)| {
    let price_a = turn.vendor_prices.get(ore_a).copied().unwrap_or(1);
    let price_b = turn.vendor_prices.get(ore_b).copied().unwrap_or(1);
    let roi_a = mine_roi(price_a, walk(*pos_a), backpack);
    let roi_b = mine_roi(price_b, walk(*pos_b), backpack);
    roi_a.partial_cmp(&roi_b)
        .unwrap_or(Equal)
        .then(walk(*pos_a).cmp(&walk(*pos_b)))  // 平局取近
        .then(pos_a.x.cmp(&pos_b.x))
        .then(pos_a.y.cmp(&pos_b.y))
})
```

**STONE 优先不变**：建墙期间 stone_demand > 0 时仍然先挖石头（石头是结构需求，不是经济选择）。

### 改动文件

| 文件 | 函数 | 行号 | 改动 |
|---|---|---|---|
| `economy.rs` | `choose_mine` | 1042-1081 | 元组排序 → ROI 排序 |
| `economy.rs` | `choose_sellable_mine` | 990-1024 | 元组排序 → ROI 排序 |
| `economy.rs` | 新增 `mine_roi` | — | 收益率计算函数 |

### 边界处理

- `trip_rounds == 0`（已在矿旁）：ROI = infinity，立即采集
- `price == 0`（无商人数据）：回退到 `price = 1`（现有行为）
- 停机矿石、已声明矿点：维持现有过滤

---

## 维度二：围墙建造顺序优化

### 现状

`route::entrance()` 已动态选门（按 errand 加权最小化行程成本）。`build_order()` 从门出发按 `build_rank` 排序环格。**这个维度已较完善。**

### 改进：经济走廊优先

当前 `build_order` 从门出发沿环走，但方向是固定的（顺时针/逆时针）。改进为：**优先建门→最近矿区方向上的墙段**，因为这条走廊是白天往返最频繁的路径，先封住它就先减少被夜间机器人切入经济走廊的风险。

```rust
// 在 build_order 中，计算每个环格到当日主矿方向的对齐度
// gate 侧的环格优先级提升（已有），矿区方向的环格额外加分
fn corridor_priority(pos: Pos, gate: Pos, mines: &[(Pos, String)]) -> i32 {
    let mine_direction = avg_mine_direction(gate, mines);
    let wall_direction = direction_from(gate, pos);
    // 对齐度越高（墙在矿区方向），优先级越高
    dot_product(mine_direction, wall_direction)
}
```

### 改动文件

| 文件 | 函数 | 行号 | 改动 |
|---|---|---|---|
| `route.rs` | `build_order` | ~80 | 排序键加入走廊方向权重 |

### 评估

**优先级低**。现有门选择已考虑矿区，建墙顺序的边际改进很小。如果时间有限，跳过此维度。

---

## 维度三：夜间火力分配优化

### 现状

1. 塔按 `threat_load` 降序排序，依次开火
2. 共享 `Sim` 做 HP 预留——后开的塔看到前塔造成的伤害
3. 每个塔独立最大化 `hit_value`（击杀分 + 资产损害 + 紧迫性 + 威胁）
4. **无显式目标分配**——贪婪选择导致 overkill

### 问题示例

3 座塔（各 10 伤害），2 个机器人（HP 15, HP 20）：
- 当前：塔A→机器人1(15HP, 杀), 塔B→机器人2(20HP, 残10HP), 塔C→机器人2(杀)。总击杀 2，浪费 5 伤害
- 最优：塔A+B→机器人2(20HP, 杀, 浪费0), 塔C→机器人1(15HP, 杀)。总击杀 2，浪费 5 伤害

更极端：3 塔（各 10 伤害），1 机器人（HP 10）：
- 当前：塔A→机器人(杀)，塔B/C→无目标。浪费 0，但 B/C 本可打建筑
- 最优：同上——Sim 已处理此情况

**核心问题**：当多个塔面对少量高 HP 目标时，贪婪选择可能集中火力但浪费 overkill。显式分配能确保火力分散到最大化击杀数。

### 设计：贪心目标分配 + Overkill 检测

不用匈牙利算法（问题规模小，收益不显著）。改为在现有 Sim 基础上加 **overkill 感知**：

```rust
// combat.rs - 修改 hit_value，加入 overkill 惩罚
fn hit_value(turn, robot, hp_before, dmg) -> f64 {
    let mut value = dmg as f64 * DAMAGE_WEIGHT;
    if dmg >= hp_before {
        // 击杀：完整 win_value，但惩罚超出 HP 的浪费
        let overkill = dmg - hp_before;
        value += win_value(turn, robot) as f64;
        value -= overkill as f64 * OVERKILL_PENALTY;  // 新增
    } else {
        value += win_value(turn, robot) as f64 * dmg as f64 / hp_before as f64 / 4.0;
    }
    value
}
```

**效果**：当塔的齐射伤害远超目标 HP 时，`hit_value` 下降，该目标变得不如其他目标有吸引力。塔自然转向其他目标，减少 overkill。

### 设计：武器-目标匹配优化

当前三种武器各自独立选择目标。加入武器特性偏好：

| 武器 | 最佳目标 | 原因 |
|---|---|---|
| Rocket (splash) | 密集集群 | 3×3 溅射，多目标收益最大化 |
| Railgun (piercing) | 直线排列目标 | 穿透伤害，一线多杀 |
| Gatling (multi-hit) | 单体高价值 | 多子弹集中一个方向 |

**实现**：在 `choose_attack_kind_with` 中，为每种武器加入偏好权重：

```rust
// Rocket: 已有 splash_value，天然偏好集群 — 无需改
// Railgun: 已有穿透累积 value — 无需改
// Gatling: 在 choose_gatling 中，对孤立高价值目标加成
fn choose_gatling(turn, tower, robots, bullets, sim) {
    // 现有逻辑 + 对周围8格内无其他机器人的目标加 1.2x 系数
    let isolation_bonus = if is_isolated(robot, robots) { 1.2 } else { 1.0 };
    value *= isolation_bonus;
}
```

### 改动文件

| 文件 | 函数 | 行号 | 改动 |
|---|---|---|---|
| `combat.rs` | `hit_value` | 327 | 加入 overkill 惩罚项 |
| `combat.rs` | `choose_gatling` | 524 | 加入孤立目标偏好 |
| `combat.rs` | 新增 `OVERKILL_PENALTY` | — | 常量 = 0.5（半价值惩罚） |

### 评估

**优先级高**。Overkill 惩罚是低风险高收益改动——只改排序权重，不改控制流。武器-目标匹配是锦上添花。

---

## 实施顺序

1. **维度一（采矿 ROI）** — `economy.rs` 2 个函数改排序逻辑 + 1 个新增辅助函数
2. **维度三（火力优化）** — `combat.rs` `hit_value` 加 overkill 惩罚 + gatling 孤立偏好
3. **维度二（围墙顺序）** — `route.rs` 排序键加入方向权重（可选，优先级低）

## 测试策略

- 维度一：新增 `mine_roi_test.rs`，验证远距离高价值矿胜过近距离低价值矿
- 维度三：修改 `combat.rs` 测试中的 overkill 场景，验证惩罚生效
- 现有测试全部通过（回归保护）
