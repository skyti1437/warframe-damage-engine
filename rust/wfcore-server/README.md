# wfcore-server

Phase 2a 独立服务。工作区发行版构建命令为 `python tools/build_rust_workspace.py --offline --server-only`。运行 `rust/target/release/wfcore-server --probe` 检查数据和目录，运行 `rust/target/release/wfcore-server --port 8765` 启动 HTTP 服务。Windows 的二进制带 `.exe` 后缀。

`--data-dir` 接受项目根或 `data` 目录。省略时从当前目录及可执行文件位置向上查找。默认监听 `127.0.0.1:8765`，`WF_SERVER_THREADS` 可覆盖两个 HTTP 工作线程的默认配置。健康指纹由加载的数据文件和内置兼容规则计算。

Rust 在启动时应用快照修正、合并条目、计算公开标记并序列化目录。`--dump-catalog` 输出与 HTTP 共用的目录缓存，供禁止监听端口的环境对拍。`EngineData` 保留路径索引与战斗输入，后续计算路由可复用。

## 冻结的目录兼容规则

`compat/` 对应 Phase 1 提交 `53ae22f` 和导出版本 `2026.08.13.13.24`。这些规则随二进制编译。

- `weapon_metrics.json` 保存 `_metric_availability` 的无配装指标基线，按武器路径、开火模式及指标顺序排列。需要覆盖率或不可用时，记录基线、原因和覆盖率标志。
- `weapon_rules.json` 保存 `_weapon_build_issue` 的配卡例外、同一武器的架势优先级，以及显赫武器在 100% 强度下的目录修正及关联战甲展示。
- `incarnon_details.json` 保存 `_incarnon_options` 中依赖计算模型的效果说明和未建模提示。路线、名称与推荐项仍取自灵化导出、文本及默认策略。
- `mod_rules.json`、`arcane_rules.json`、`riven_rules.json` 和 `factions.json` 保存 Python 规则的卡片修正、支持表、套装及词条映射、赋能控制和派系回退值。`display.json` 保存公开的外部增益与哨兵展示信息。两套目录在 Python 测试中按全部 JSON 字段对拍。

对应的伤害、配卡和事件实现迁移后，可以用共享的 Rust 计算替换冻结基线和计算标记。更新快照或补丁时应重新对拍上述规则。

## HTTP 传输

`vendor/tiny_http` 来自本机已缓存的 `tiny_http 0.12.0`。它保留 MIT 和 Apache-2.0 许可，并增加长度上限入口。在为请求分配或读取请求体之前，它将超限请求的原始头交给路由以生成 JSON 错误，随后关闭连接。
