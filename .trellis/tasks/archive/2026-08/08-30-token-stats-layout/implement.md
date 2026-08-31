# 实施计划

1. 将 Token 页外层容器、页头、更新时间、刷新按钮和来源 Tabs 对齐积分统计页。
2. 将总览改为外部区块标题 + 四列指标卡，并统一指标图标、字体和分隔线。
3. 重构趋势区块：标题移到卡片外；描述、四项日期筛选放入 `CardHeader`；前端过滤 daily 数据并显示范围合计。
4. 将 Token 构成、热力图、用量分布和高消耗会话改为外部标题 + 内部内容卡，移除重复卡内标题。
5. 统一页面纵向间距、卡片圆角、边框、空状态和窄窗口换行；保留来源隔离、分布维度切换和缓存写入提示。
6. 使用 screenshot demo 启动本地页面，在 Browser 中检查标题位置、日期按钮、来源切换、滚动和窄窗口无横向溢出。
7. 运行 `npm run build`、`cargo test --workspace`、`git diff --check`；由 Trellis checker 复核视觉结构、状态与范围逻辑。
8. 根据审查结果修复，更新相关 UI 规范（若产生可复用约定），提交中文 Conventional Commit，归档任务并记录会话。

## 回滚点

- 产品代码预计只修改 `src/pages/TokenStatsPage.tsx`；若需要 demo fixture 测试调整，限制在 `src/lib/screenshot-demo.ts`。
- 不修改 Rust 聚合器、API 路由和 TypeScript 响应契约。
