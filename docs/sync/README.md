# Zed 同步文档

同步或审计上游 GPUI 时，首先阅读 [`CURRENT.md`](./CURRENT.md)。它是当前唯一可执行的结构映射、分歧决策和验证说明。

## 当前入口

- [`CURRENT.md`](./CURRENT.md)：当前上游基线、crate/path 映射、明确分歧、同步流程和有效命令。
- [`upstream-audit.json`](./upstream-audit.json)：上一完整分类区间的逐提交分类，外加区间之后的 cherry-pick 清单。`audited_upstream` 不是“已全部吸收的祖先”。
- [`ZED_UPSTREAM_MIGRATION_REVIEW_2026-08-10.md`](./ZED_UPSTREAM_MIGRATION_REVIEW_2026-08-10.md)：上一轮完整分类（到 `4bd19937`）的价值审计与分阶段路线。
- [`ZED_GPUI_INCREMENTAL_AUDIT_2026-07-01.md`](./ZED_GPUI_INCREMENTAL_AUDIT_2026-07-01.md)：自指定基线开始的历史增量审计。
- [`ZED_GPUI_P1_BACKPORT_2026-07-20.md`](./ZED_GPUI_P1_BACKPORT_2026-07-20.md)：P1 backport 和当时的平台验证记录。

## 历史指南

以下文档描述 crate 拆分前的结构，保留用于追溯，不应直接照着执行：

- [`ZED_SYNC_MAPPING.md`](./ZED_SYNC_MAPPING.md)
- [`QUICK_SYNC_REFERENCE.md`](./QUICK_SYNC_REFERENCE.md)
- [`TECHNICAL_SYNC_GUIDE.md`](./TECHNICAL_SYNC_GUIDE.md)
- `BATCH*`、`SUMMARY.md` 等带批次或日期的历史报告

历史报告中的路径、包名和命令可能已经失效。不要通过修改历史报告来表达当前状态；在 `CURRENT.md` 更新当前结论。

## 维护规则

1. 只在完成一轮**完整**增量分类后更新 `audited_upstream` 和 `CURRENT.md` 的分类终点。主题 cherry-pick 追加 `post_audit_backports`，不要把未分类 HEAD 写成已吸收祖先。
2. 每个 backport 提交记录完整 `Zed-Origin`。
3. 对未迁移提交明确标记 `equivalent`、`deferred` 或 `not-applicable`。
4. 核心测试使用 `fc-gpui-core`，兼容测试使用 `fc-gpui`。
5. 使用 `scripts/verify-upstream-sync.sh` 校验分类完整性和 `Zed-Origin` 来源追踪。
6. 完整闭环运行 `scripts/verify-migration.sh`。
