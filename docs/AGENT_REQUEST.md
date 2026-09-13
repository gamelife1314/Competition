# 给自动对战分析 Agent 的请求

> 本文档同时作为 git commit 的载体：`git log` 中可见完整请求。
> 目标读者：负责分析对战日志、生成「改进任务」issue 的内部 code agent。

## 请求内容：issue 头部补充累计战绩字段

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

## 为什么需要

单场失利可能是运气（对手强弱、出生点、地图随机）；只有**排名与胜率的长期趋势**才能
说明改进是否真的有效。有了这些数据：

1. 能判断某次改进后胜率是上升还是下降，避免"修了半天反而更差"
2. 连续多场失利时可识别系统性问题（优先修稳定性而不是抠细节）
3. 排名变化可直接作为改进优先级的依据

## 机器可读请求块

```yaml
agent_request:
  target: issue-generator
  repo: gamelife1314/Competition
  action: extend_issue_header
  version: 1
  add_fields:
    - name: current_rank
      desc: rank among all teams
      example: "12 / 64"
    - name: total_wins
      desc: cumulative match wins
      example: 8
    - name: total_losses
      desc: cumulative match losses
      example: 5
    - name: total_draws
      desc: cumulative match draws
      example: 1
    - name: win_rate
      desc: wins / total matches, percent, 1 decimal
      example: "57.1%"
  placement: after "生成时间" line
```

## English summary

Please extend the auto-generated improvement-issue header with cumulative
battle statistics: **current rank**, **cumulative wins**, **losses**,
**draws**, and **win rate** (wins / total matches, one decimal place).
Single-battle results cannot show whether a code change actually improved
overall performance; rank and win-rate trends can.

---

*本文件由本地 agent 添加，用于向内部 code agent 传递字段需求。*
