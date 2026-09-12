# CoreGeek — 《未来战争》Rust 参赛 Bot

云核心网第十届编程大赛《未来战争》参赛实现。Rust **1.72**（`rust-toolchain.toml` 已锁定），
HTTP 服务形态：判题器每回合 POST 局面 JSON，本程序在 5 秒时限内返回全角色指令。

## 构建与运行

```bash
cargo build --release --locked   # 使用 1.72 工具链（rust-toolchain.toml 自动选择）
bash run.sh 8080                 # 或 ./target/release/coregeek 8080
```

本 crate 同时是仓库根 `Cargo.toml` 声明的 workspace 成员：评审脚本在仓库根执行
`cargo test --release --locked`（不进入 `CoreGeek/`）时才能找到清单文件。因此**在
`CoreGeek/` 目录内构建请带上 `--target-dir target`**（`run.sh` 已内置），否则产物会落到
仓库根的 `target/`。（只拷贝 `CoreGeek/` 单独构建时无 workspace，产物仍在 `CoreGeek/target/`。）

依赖版本已在 `Cargo.toml` 精确锁定并随仓库提交 `Cargo.lock`
（cargo 1.72 无 MSRV 感知解析，新版传递依赖如 hashbrown 0.17/edition2024 无法编译，勿随意升级）。

## 测试

```bash
cargo test                       # 单元测试 + 回放 docs/request.txt 的集成测试
```

## 架构

```
src/
  main.rs        端口参数、同步 bind（规避 10s 连接超时）、tokio 单线程 runtime
  server.rs      hyper HTTP/1.1 服务；任何内部错误兜底返回空指令集
  protocol.rs    请求/响应 serde 模型（宽容解析）+ 指令构造器
  model.rs       Turn 局面视图：昼夜历法、切比雪夫距离、阻挡、矿区/商店/任务点
  path.rs        8 邻域 A*（走向“可动作格”集合）
  state.rs       跨回合记忆：新闻日志、停采窗口、任务会话、宝藏计划、LLM 预算、失败反馈
  validate.rs    指令合法性校验层（结构级非法指令直接丢弃，防 5 次异常判负）
  brain/
    mod.rs       decide() 主流程
    day.rs       白天：工人经济状态机、开拓者任务/宝藏/购物动线
    night.rs     夜晚：控制器↔炮塔配对、攻击、消耗品、备援经济、避难
    economy.rs   购物清单、卖矿定价、升级券使用、选矿
    combat.rs    加特林 90° 锥多目标 / 电磁炮穿透 / 火箭溅射弹道模型与目标选择
    news.rs      官方消息关键词解析（矿种停采窗口）
    task.rs      自进化任务状态机：acceptTask → prompt → executeCmd → submitAnswer，SOP 缓存复用
    treasure.rs  民间传闻累积 → LLM 推理祭坛(坐标/献祭品/开启日) → summonTreasure 反馈闭环
```

## 策略要点

- **白天**：工人优先补建三塔（加特林/电磁/火箭）与围墙环（留出入口），
  石头优先供墙、富余矿产按实时价格卖给出价最高的小贩；官方新闻解析出的停采矿种不采、存货趁涨价抛售。
  金币富余时购买武器/基地升级券并走到建筑旁使用；黄昏前 12 回合各角色预就位到自己的炮塔。
- **夜晚**：三控制器配对三炮塔（贪心最近分配，任务中的开拓者除外）；
  按武器弹道模型选目标——加特林在 90° 锥内贪心集火、电磁炮枚举穿透串、火箭对集群做溅射评分（含 3 回合冷却）；
  优先击杀威胁我方的机器人与高性价比目标，射程内无机器人时 opportunistic 打击敌方基地/单位；
  无机器人可射时优先打击可见敌方角色和武器，削弱对方火力；备援角色先治疗，再撤回墙内并使用炸弹/眩晕法宝，夜间不采矿。
- **任务**：开拓者在任务点领取自进化任务，任务期间 prompt(LLM) 不计每日额度；
  沙盒执行 → 解析 `[exitCode:N]` 输出 → 提取 `ANSWER:` 提交；答错自动带反馈重试，
  超时前强制提交当前最优答案；成功脚本沉淀为 SOP 缓存加速同类任务。
- **宝藏**：逐日累积民间传闻，用每日 3 次 LLM 额度推理祭坛坐标/献祭品/开启日，
  购买任务用品后按开启日前往献祭；结果码 2(未到时间)推迟一天重试、3(物品错误)带反馈重新推理。
- **鲁棒性**：所有指令先过校验层（角色匹配/昼夜限制/相邻性/字段完整性/锥角合法性），
  不合格指令直接丢弃——宁可空过一回合，不送异常次数；进程 panic 只损失单个连接。
