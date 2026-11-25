# wfcore-server

纯 Rust 独立服务。工作区发行版构建命令为 `python tools/build_rust_workspace.py --offline --server-only`。运行 `rust/target/release/wfcore-server --probe` 检查数据和目录，运行 `rust/target/release/wfcore-server --port 8765` 启动 HTTP 服务。Windows 的二进制带 `.exe` 后缀。

`--data-dir` 接受项目根或 `data` 目录。省略时从当前目录及可执行文件位置向上查找。默认监听 `127.0.0.1:8765`，`WF_SERVER_THREADS` 可覆盖两个 HTTP 工作线程的默认配置。健康指纹由加载的数据文件和内置兼容规则计算。

## 独立运行包

构建二进制后，从仓库根目录执行：

```sh
python3 tools/package_rust_bundle.py --verify
```

默认在临时目录生成本机平台的 `portable-rust-*.zip`。`--binary` 和 `--output` 可指定二进制及输出路径，`--metadata-out` 可生成含 SHA-256 和包大小的 JSON。运行包支持 `darwin-arm64`、`linux-x86_64` 和 `linux-arm64`。`--verify` 会在同平台隔离解包，补齐数据并执行 `--probe`。

部署时解包并按 `manifest.json` 的 `upstreamFiles` 从固定上游提交取回 42 个数据文件，保留相对路径并逐个核对 `files` 中的 SHA-256。确保 `wfcore-server` 权限为 `0755`，在解包目录运行 `./wfcore-server --probe` 或 `./wfcore-server --port 8765`。运行进程只需要本机二进制和数据文件。

冻结数据与内置规则的 FNV-1a 摘要为 `3ea749df5c3de728`，对应已发布评分指纹 `535ed49f5a51149d`。`--probe`、`/api/health` 和 `/api/analyze` 复用此评分指纹。加载的输入字节发生变化，且摘要未命中兼容表时，服务返回新的原始摘要。

Rust 在启动时应用快照修正、合并条目、计算公开标记并序列化目录。`--dump-catalog` 输出与 HTTP 共用的目录缓存，供禁止监听端口的环境对拍。`EngineData` 保留路径索引与战斗输入，后续计算路由可复用。

## 计算入口

`POST /api/simulate` 计算已装备配装。`POST /api/build` 从导出的卡池搜索配装，复用模拟的面板和事件回放，并返回所选卡片、容量、Forma 和评分。服务也提供 `GET /api/health` 和 `GET /api/catalog`。

- `POST /api/analyze` 验证紫卡词条及卡面倾向，将数值投影到目标倾向，并通过相同事件回放搜索无紫卡基线与实卡配装。硬度校准使用无紫卡基线的身体命中事件，评分汇总三个加权硬度节点。响应包含固定配装消融、词条推荐和负面评估。分析指纹来自本服务的健康端点，`contextHash` 按 Python 的规范 JSON 和 SHA-256 生成。
- `POST /api/trait-matrix` 使用配卡搜索的参考配装，按 Forma 槽位回放基线和逐槽消融，分别运行弱点和元素试算。进程缓存保留完整矩阵，`roll` 接受 `3+1-`、`2+1-` 或 `both`。

`--simulate-batch <JSON_PATH>` 和 `--build-batch <JSON_PATH>` 接受由 `{"id": "case", "body": {...}}` 条目组成的 JSON 数组。`--request-batch <JSON_PATH>` 每条另带 `method` 与 `path`，可在一个批次调用六个端点。GET 条目省略 `body`。标准输出按输入顺序给出每项的 `id`、`status` 和 `response`。每个请求体与 HTTP 使用相同的 128 KiB 上限和校验。显赫武器的战甲联动配卡仍依赖战甲计算器。

## 冻结的目录兼容规则

`compat/` 对应 Phase 1 提交 `53ae22f` 和导出版本 `2026.08.13.13.24`。这些规则随二进制编译。

- `weapon_metrics.json` 保存 `_metric_availability` 的无配装指标基线，按武器路径、开火模式及指标顺序排列。需要覆盖率或不可用时，记录基线、原因和覆盖率标志。
- `weapon_rules.json` 保存 `_weapon_build_issue` 的配卡例外、同一武器的架势优先级，以及显赫武器在 100% 强度下的目录修正及关联战甲展示。
- `incarnon_details.json` 保存 `_incarnon_options` 中依赖计算模型的效果说明和未建模提示。路线、名称与推荐项仍取自灵化导出、文本及默认策略。
- `mod_rules.json`、`arcane_rules.json`、`riven_rules.json` 和 `factions.json` 保存 Python 规则的卡片修正、支持表、套装及词条映射、赋能控制和派系回退值。`display.json` 保存公开的外部增益与哨兵展示信息。两套目录在 Python 测试中按全部 JSON 字段对拍。

配卡与模拟共享磁力破盾回放。`magnetic_weakness.json` 保留 WFSim `27f4b19fa3` 的原始武器弱点标量，来源与 Python 参考实现相同。更新快照或补丁时应重新对拍上述规则。

`fire_rate_spool.json` 保留 Python 参考的射速预热档案及其来源标识。回放由共享的 `wfcore-lib` 执行。

## HTTP 传输

`vendor/tiny_http` 来自本机已缓存的 `tiny_http 0.12.0`。它保留 MIT 和 Apache-2.0 许可，并增加长度上限入口。在为请求分配或读取请求体之前，它将超限请求的原始头交给路由以生成 JSON 错误，随后关闭连接。
