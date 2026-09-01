# 技术设计

## 页面骨架

以 `CreditStatsPage` 为结构基准，不复制 Token 页现有的紧凑 dashboard 外壳：

1. 页面容器统一为 `max-w-[1180px]`、`px-4/sm:px-8`、`py-6/sm:py-9`。
2. 页头使用相同的外部标题、更新时间说明和右侧操作区；Token 来源 Tabs 与刷新按钮并排放在右侧。
3. 主内容使用 `space-y-12` 的纵向区块。每个区块都采用 `section > 外部 h2 > Card`，卡片内仅保留说明、筛选和数据内容。
4. 总览改为积分统计同款四列指标卡：总 Token、输入 Token、输出 Token、缓存命中率。
5. 趋势、Token 构成、使用热力图、用量分布、消耗最高的会话依次作为独立区块，保留 Token 专属图表和语义。

## 筛选与状态

- 页面级 `source`：WorkBuddy / CodeBuddy CLI，控制整页数据源。
- 趋势卡片级 `range`：近 30 天 / 今天 / 近 7 天 / 本月，仅控制趋势图和趋势卡片底部范围汇总，行为与积分统计 `TrendChart` 一致。
- 用量分布卡片级 `distribution`：按项目 / 按模型，继续放在该卡片内部。
- 刷新按钮重新读取一次完整本地历史聚合；日期切换只做前端 `daily` 投影，不重复扫描磁盘。
- 加载、错误、空数据沿用积分统计的页级 Alert/Loader 和卡片内虚线空状态。

## 数据流

`getTokenStatistics()` 一次返回完整历史的双来源聚合 → 页级来源 Tab 选择 `TokenStatsSource` → 总览/构成/热力图/排行直接使用当前来源 → 趋势卡片按本地日期从 `source.daily` 筛选四种范围。

本轮不修改 Rust、Tauri 或 HTTP 接口。现有 `rangeDays` 参数保留兼容，但新版 Token 页不再通过日期按钮传递它。

## 组件复用

- 继续复用 `Card`、`Button`、`Tabs`、`ChartContainer`、`ChartTooltip`。
- 指标卡与区块标题的 class 直接对齐 `CreditStatsPage`；不抽取跨页面共享组件，避免本次视觉统一演变成高风险的大范围重构。
- 不新增自定义交互原语。

## 风险与回滚

- 完整历史数据可能比 30 天响应稍大，但聚合仍在 Rust 侧完成，前端只接收汇总数组；避免日期切换触发重复磁盘扫描。
- 本月按本地时区的 `YYYY-MM` 过滤，与 Rust 生成 daily key 的本地时区保持一致。
- 如出现布局回归，单文件回滚 `TokenStatsPage.tsx` 即可；本轮不改变后端契约和原始统计口径。
