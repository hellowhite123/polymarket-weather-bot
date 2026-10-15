# db/migrations — 迁移约定

- 文件命名：`NNNN_name.up.sql` / `NNNN_name.down.sql`，版本号单调递增。
- **已合入的 migration 禁止修改**；变更只能新增更高版本的 migration。
- 每个 up migration 末尾登记 `schema_migrations`；down 负责对应删除。
- 执行器：本轮未定（候选：sqlx-cli / alembic / 自研 runner），见
  `docs/design/trading_database_integration_plan.md`。
- ⚠️ 所有 migration 仅在**独立测试库**验证；真实库的首次执行需伯森先生显式授权。
