# audits/ 目录说明

本目录存放**每一轮代码审计的完整档案**，永久保留，不可删除。

## 命名约定

```text
audits/
├── round_01/
├── round_02/
├── round_03/
├── round_04/
└── round_N/
```

每轮（Round）目录内至少包含：

| 文件 | 产出方 | 说明 |
|------|--------|------|
| `audit_gpt_round_N.md` | GPT | 系统总审计 / 主架构师 |
| `audit_gemini_round_N.md` | Gemini | 第二架构师 / 反方 / Red Team |
| `audit_quant_round_N.md` | DeepSeek | Quant / 数学与策略审计 |
| `audit_security_round_N.md` | KIMI | Security / DevOps |
| `CONSENSUS_ROUND_N.md` | Judge | 共识与最终 Severity 裁决 |

## 独立审计原则（协议 §9）

- 四家 AI **独立并行**读取同一版本代码，产出各自 `audit_*.md`，互不参考。
- 审计输入固定（commit / 代码快照 + 文档版本），避免中途变化。

## Finding 与状态机

- Finding 格式、Severity 判据、六态修复状态机见 `docs/CODE_REVIEW_STANDARD.md`。
- 每轮结论汇总进 `CONSENSUS_ROUND_N.md`，可执行项进入 `MASTER_TASKS.md`。

## 历史轮次

| Round | 结论 | 备注 |
|-------|------|------|
| 01 | — | 待归档 |
| 02 | — | 待归档 |
| 03 | NO-GO（9 项 P0 待闭环） | 见 `../review.md` 与项目记忆 |
| 04 | 待启动 | 用新标准对 GPT P0 清单做首轮独立验证 |
