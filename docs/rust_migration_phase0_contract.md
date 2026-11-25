# Rust 独立化路线 A · Phase 0 契约

基线为 `feat/rust-standalone-phase0` 的 `00ebdd6`，发布标识为 `portable-535ed49f5a51149d`。发布入口是 `launch.py`。测量使用 Python 3.12.15、单搜索工作进程和该提交对应的 `wfcore`。发布入口读取仓库中的分析静态缓存并关闭落盘。

## HTTP 公共格式

成功响应为 HTTP 200 和 UTF-8 JSON 对象。两个 GET 不需要请求体，四个 POST 接收 UTF-8 JSON 对象。发布入口响应的 `Content-Type` 为 `application/json; charset=utf-8`。目录使用 `Cache-Control: no-cache`，健康检查、成功的 POST 和错误使用 `no-store`。

| 端点 | 请求 | 客户端消费的主要响应字段 |
| --- | --- | --- |
| `GET /api/health` | 无参数 | `ok: true`、`buildLabel`、`engineFingerprint`、`schemaVersion`。发布入口在监听前核查数据版本、输入指纹以及原生扩展是否启用。 |
| `GET /api/catalog` | 无参数 | `buildLabel`、`schemaVersion`、`weapons`、`manualMods`、`arcanes`、`enemies`、`counts`、`manualCounts`。另有 `statsByType`、`stances`、`utilityMods` 和 `buildOptions` 等供选择器和编辑器读取。 |
| `POST /api/simulate` | `weapon`、`fireMode`、目标和战斗场景。可指定 `mods`、`slotCount`、`slotPolarities`、`riven`、`exilus`、`arcanes`、`stancePath`、`incarnonChoices`、`forcedProcs` 和 `metric`。`damageModel` 支持 `target`（默认）、`panel` 和 `custom`。选 `target` 时须提供完整的目录敌人，选 `custom` 时须提供 `defenseSnapshot` 或 `customDefense`。 | `selection`、`loadoutValidation`、`moddedStats`、`targetDamage`、`killMetrics`、异常和持续伤害分项、`incarnonChoices`、`arcanes`、`exilus`、`diagnostics`。目标事件回放由 `benchmarkTimeline` 给出，含 `windowDps`、`killsPerMinute`、`transforms`、`directWindow`、`dotWindow`、`points`、`stacks` 和目标上下文。依据场景可出现 `headshotMix`、`timeline` 和近战、弹道等专项回放字段。 |
| `POST /api/build` | `weapon`、`fireMode`、`metric`、目标、场景、`slotCount`（1–8，默认 8）。可指定 `requiredMods`、`forbiddenMods`、`minFill`、`maxFill`、`excludeFactionMods`、`elementTarget`、`riven`、`incarnonChoices`、`autoIncarnon`、`exilus`、`autoExilus`、`arcanes`、`autoArcane` 和 `optimizeWithArcanes`。 | `evaluationTarget`、`evaluationContext`、`evaluationScenario`、`scoringModel`、`scoreMetric`、`build.objectiveScore`、`build.mods`、可选 `build.riven` 及其槽位、容量、架势、扩展槽、极化、灵化选择、`autoLoadout` 和 `moddedStats`。黄金伯斯顿平手用例固定对照路线的其他进化档，并锁定自动档 `tier2: "2-1"`。 |
| `POST /api/analyze` | `weapon`、`disposition`、`stats`。可指定 `sourceDisposition`、`rank`、`fireMode`、`profile`、`scenario`、`benchmarkTarget`、`incarnonChoices`、`stancePath` 和 `exilus`。分析扩展槽可用 `{"mode":"auto"}` 或固定卡对象。`disposition` 范围为 (0, 2]。词条包含 `code`、正幅值 `value`、可选布尔 `negative`，允许 2 或 3 正、最多 1 负。融合词条仅正面且最多 1 条。 | `contextHash`、`evaluationTarget`、`evaluationScenario`、`targetCalibration`、`scoringValues.baseline`、`scoringValues.actual`、`actualGain`、`baseline`、`actual`、`details`、推荐正负词条、致命负面、`incarnonChoices`、`searchWork` 和版本字段。 |
| `POST /api/trait-matrix` | 共用配装的武器、目标、场景、架势、扩展槽、赋能和灵化选择。接受 `roll: "3+1-"`、`"2+1-"` 或 `"both"`（默认）。计算无紫卡基线和合法单词条试算，固定采用目标模型和 `killsPerMinute`。 | `weapon`、`fireMode`、`evaluationTarget`、`evaluationScenario`、`build`、`matrix`。基线带 DPS、KPM 和弱点口径。正负数组按 `code` 排序，每行含 `stat_id`、`unit`、建模标志、`shown_31`、`pct_31`、`shown_21`、`pct_21` 和元素配卡 `elem_build`。单视图把另一侧的展示值和百分比置为 `null`，完整保留对应字段。 |

- 标准靶为 `/Lotus/Types/Enemies/Duviri/Avatars/DuviriMeleeAvatar`，170 级钢路，`enemyEnhancements.noOverguard: true`、`shieldOverride: 0`、`headshotChance: 0.0`、`windowSeconds: 30`。三靶比较使用同一 Braton Prime 和相同窗口，切换超宏或护盾，另加 9999 级、带超宏、100% 爆头、180 秒用例。
- 目标字段包括 `enemy`、非负整数 `level`、布尔 `steelPath`、`headshotChance`（0–1）、`windowSeconds`（0.1–600）、`armourStrip` 和 `elementalArmourStrip`（0–1）。`enemyEnhancements` 使用 `noOverguard`、`shieldOverride`、`eximus` 等。配装、分析和矩阵还可使用 `benchmarkTarget`，内层可写 `enemy` 或 `path`。
- 场景使用 `aiming`、`incarnonCycle`、`heavyAttack`、`comboTier` 和 `combatState`。`combatState.gaugeChargingHeadshots` 是严格布尔值，只影响攒条判定。实际伤害爆头率与爆头触发仍由 `headshotChance` 决定。近战的 `combatState.meleeRoute` 覆盖 `combo`、`slamBullet` 和 `glaive`。
- MOD 条目用 `path`、`rank`、从 0 开始的 `slot`。自定义紫卡的 `stats` 使用词条代码和数值，模拟允许附 `slot` 和 `polarity`。扩展槽用 `path` 和 `rank`，独立于普通槽。赋能用 `path`、`rank`，可显式传 `stacks` 和 `uptime`。灵化进化用如 `{"tier2":"2-1"}` 的映射。
- 搜索档位为 `ultra`、`fast`、`standard`、`deep`。冻结配装和矩阵用 `fast`，分析用 `ultra`。为了让黄金套件执行稳定，常规推荐限制普通槽数和战斗窗口，平手对照用裸枪。全词条矩阵仍遍历合法候选。

`buildLabel` 为 `2026.08.13.13.24`，`schemaVersion` 为整数 `3`。这两个字段出现在健康、目录、模拟、配装和分析响应。`engineFingerprint` 为字符串 `535ed49f5a51149d`，由健康和分析返回。分析还返回整数 `engineCacheVersion: 26` 和随请求评估上下文变化的 `contextHash`。矩阵现有响应保持表中的六个顶层字段。

### 错误口径

POST 的统一错误体是 `{"error":{"code":"…","message":"…"}}`，求解参数错误可能增加 `error.candidates`。首批黄金用例冻结成功响应，表内错误来自发布路由定义。

| HTTP | `error.code` | 条件 |
| --- | --- | --- |
| 400 | `invalid_length` | 缺少或无法解析 `Content-Length`。 |
| 400 | `invalid_json` | JSON 解码失败或请求体包含无效 UTF-8。 |
| 404 | `not_found` | POST 路径未注册。 |
| 409 | `superseded` | 同一页面的更新请求取代当前请求。 |
| 413 | `request_too_large` | 体积不在 1–131072 字节内。 |
| 422 | `invalid_request` | 对象形状、字段类型、范围、武器与配装约束或规则校验失败。 |
| 500 | `internal_error` | 求解发生未预期异常，公开消息固定为 `analysis failed unexpectedly`。 |

### 对拍规则

夹具格式版本是 `fixtureVersion: 1`。套件定义了 24 个用例，涵盖 2 个 GET、13 个模拟、4 个配装、2 个分析和 3 个词条矩阵。每个用例写入 `id`、`method`、`path`、可选 JSON `body` 与期望 HTTP 状态和响应。验证器可用 `--verify --url http://127.0.0.1:8765` 对任意运行中的兼容服务执行。省略 URL 时启动新的发布子进程。校验失败会输出用例、端点和 JSON 字段路径，并返回非零。

- HTTP 状态、JSON 类型、对象键集合、列表长度和顺序均要匹配。字符串、布尔、整数、`null`、MOD 路径、槽位和词条严格对拍，对象键的顺序可变。JSON 的 `1` 与 `1.0` 类型不同。
- 双方均为有限浮点数时，允许 `abs(a-b) <= max(1e-9, 1e-9 * max(abs(a), abs(b)))`。
- 目录摘要读取原始数组长度，映射为 `weapons`、`mods`、`arcanes`、`enemies` 四个计数，并保存代表条目全部字段和相对顺序。该版本对应 684 把武器、406 张 MOD、68 项赋能和 1351 个敌人。

| JSON 字段 | 处理方式 |
| --- | --- |
| 模拟根级 `exportDir` | 剥离宿主机绝对数据路径。 |
| 配装 `autoLoadout.seconds`、`autoLoadout.incarnon.seconds` | 剥离自动配装和灵化分支筛选的运行耗时。 |
| 配装 `skipped[]` 中以 `optimizer-auto-incarnon: ` 开头的条目 | 仅把「次配装搜索」旁的秒数片段替换为 `<耗时>`，保留次数、路线和增幅。 |
| 分析 `searchWork.elapsedSeconds`、`searchStatus.work.elapsedSeconds`、`incarnonRouteScreen.seconds` | 剥离墙钟耗时，含分析状态中的副本。 |
| 分析根级 `responseCache` | 剥离本进程的响应缓存命中状态。 |

`windowSeconds`、`durationSeconds`、`fieldDuration`、触发时间和时间线坐标属于战斗数据，参与对拍。`searchWork` 的搜索次数、预算及限时标志，以及所有分数、指纹和版本字段同样参与对拍。

## 性能与内存基线

`--profile` 在新的发布进程中记录从创建进程到首次健康检查成功的墙钟时间。启动 RSS 取自首个健康响应后，负载后 RSS 取自各类型首轮请求完成后。两次均从 `ps -o rss= -p <pid>` 读取 KiB 并换算成 MiB。HTTP 延迟覆盖建立连接、发送请求、接收完整响应，计时结束后再验证响应内容类型。每项测一次首次请求，随后对相同请求再采样三次，给出热态 P50 和最大值。重复配装、分析和词条矩阵会使用请求级缓存。六个端点共测七组请求，其中配装的有紫卡和无紫卡两组共用同一目标、档位、自动扩展槽和手动赋能。

以下启动、RSS 与 HTTP 延迟指标在 macOS arm64（Python 3.12.15）通过 `tools/phase0_benchmark_and_record.py --profile` 实测获得（独立启动 `launch.py` 子进程，走真实 `127.0.0.1` HTTP 环回与 `ps -o rss=`）。

| 启动指标 | 数值 |
| --- | ---: |
| 启动到首个健康响应 | 0.676 s（675.8 ms） |
| 启动后 RSS | 227.28 MiB |
| 首轮负载后 RSS | 203.06 MiB |

| HTTP 请求 | 首次延迟 ms | 热态 P50 ms | 热态最大 ms | 响应字节 |
| --- | ---: | ---: | ---: | ---: |
| `health` | 0.63 | 0.35 | 0.48 | 100 |
| `catalog` | 25.45 | 24.74 | 25.14 | 6,439,366 |
| `simulate`（伯斯顿完整配装） | 90.25 | 82.16 | 84.33 | 154,937 |
| `build(fast)`（伯斯顿无紫卡） | 1,256.72 | 0.41 | 0.54 | 12,106 |
| `build(riven)`（伯斯顿含紫卡） | 713.24 | 0.34 | 0.37 | 12,040 |
| `analyze`（伯斯顿 `ultra`） | 329.93 | 1.12 | 1.17 | 17,600 |
| `trait-matrix`（重击震地） | 1,415.81 | 0.44 | 0.45 | 16,391 |

### 函数剖析

每项在独立 Python 进程中从发布入口加载配置，预加载数据和发布静态缓存，再用 `cProfile` 直接剖析一次处理函数。占比按排除子调用的 `tottime` 计算，具有 `wfarsenal.wfcore` 名称的原生入口归到 Rust，其余归到 Python 和其他耗时。后者包括标准库 C 调用，以及剖析器无法单独识别的扩展工作。累计时间 `cumtime` 包含子调用，只用于识别高成本调用链。`native_replay.STATS` 记录该请求的计数增量，`fallback` 覆盖原生适配器内的回退分支。兼容性检查后直接走 Python 的回放也可能不产生该计数。

以下数据在 macOS arm64 和 Python 3.12.15 下通过四个独立进程实测。函数表中的 `wfarsenal/` 为仓库内的 `vendor/wfarsenal/`，标准库省略了宿主机安装路径。

| 请求 | 剖析墙钟 s | Python 及其他 s | Python 及其他占比 | `wfcore` s | `wfcore` 占比 | `native` | `fallback` |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `build(riven)` | 1.279 | 0.935 | 73.15% | 0.343 | 26.85% | 1206 | 0 |
| `analyze` | 0.651 | 0.538 | 82.67% | 0.113 | 17.33% | 240 | 0 |
| `trait-matrix` | 2.827 | 2.507 | 88.68% | 0.320 | 11.32% | 2899 | 0 |
| `simulate` | 0.285 | 0.282 | 99.19% | 0.002 | 0.81% | 0 | 0 |
| 四项合计 | 5.041 | 4.262 | 84.56% | 0.778 | 15.44% | 4345 | 0 |

#### `build(riven)` 前 15 项

| 排位 | 按 `tottime` 的函数 | 自身 s | 按 `cumtime` 的函数 | 累计 s |
| ---: | --- | ---: | --- | ---: |
| 1 | `<built-in method wfarsenal.wfcore.trajectory>` | 0.2972 | `app.py:3785(build_weapon_route)` | 1.2786 |
| 2 | `<method 'get' of 'dict' objects>` | 0.0983 | `app.py:4127(build_weapon_payload)` | 1.2785 |
| 3 | `wfarsenal/manual_combat.py:253(prepare)` | 0.0839 | `app.py:4546(search)` | 1.1086 |
| 4 | `<built-in method wfarsenal.wfcore.apply_mods>` | 0.0446 | `wfarsenal/search_session.py:119(run)` | 1.1051 |
| 5 | `<built-in method _pickle.dumps>` | 0.0416 | `wfarsenal/optimizer.py:2795(_optimize)` | 1.1050 |
| 6 | `wfarsenal/simulator.py:237(<genexpr>)` | 0.0341 | `wfarsenal/optimizer.py:2655(_evaluate)` | 1.0941 |
| 7 | `wfarsenal/runtime_adapters.py:1100(<genexpr>)` | 0.0289 | `wfarsenal/optimizer.py:2208(optimize_for_element)` | 1.0523 |
| 8 | `wfarsenal/manual_combat.py:817(resolve)` | 0.0277 | `wfarsenal/optimizer.py:2057(_score)` | 0.9361 |
| 9 | `wfarsenal/record_keys.py:16(record_key)` | 0.0246 | `wfarsenal/optimizer.py:2011(_event_window)` | 0.9258 |
| 10 | `<built-in method builtins.any>` | 0.0233 | `wfarsenal/search_combat.py:100(window)` | 0.9240 |
| 11 | `标准库/dataclasses.py:1540(replace)` | 0.0227 | `wfarsenal/combat_replay.py:73(replay)` | 0.5518 |
| 12 | `wfarsenal/runtime_adapters.py:810(verified_incarnon_condition)` | 0.0227 | `wfarsenal/combat_replay.py:184(_trajectory)` | 0.5469 |
| 13 | `wfarsenal/mods_native.py:266(apply)` | 0.0205 | `wfarsenal/native_replay.py:486(trajectory)` | 0.5337 |
| 14 | `wfarsenal/native_replay.py:486(trajectory)` | 0.0203 | `<built-in method wfarsenal.wfcore.trajectory>` | 0.4944 |
| 15 | `<method 'casefold' of 'str' objects>` | 0.0193 | `wfarsenal/manual_combat.py:253(prepare)` | 0.3079 |

#### `analyze` 前 15 项

| 排位 | 按 `tottime` 的函数 | 自身 s | 按 `cumtime` 的函数 | 累计 s |
| ---: | --- | ---: | --- | ---: |
| 1 | `<built-in method wfarsenal.wfcore.trajectory>` | 0.0500 | `app.py:3353(analyze_payload)` | 0.6507 |
| 2 | `<method 'get' of 'dict' objects>` | 0.0346 | `app.py:3559(solve_analysis)` | 0.6467 |
| 3 | `<built-in method wfarsenal.wfcore.apply_mods>` | 0.0315 | `wfarsenal/search_session.py:43(run)` | 0.6467 |
| 4 | `<built-in method _pickle.dumps>` | 0.0288 | `wfarsenal/optimizer.py:5019(analyze_riven_request)` | 0.6465 |
| 5 | `wfarsenal/optimizer.py:3212(<genexpr>)` | 0.0285 | `wfarsenal/optimizer.py:2655(_evaluate)` | 0.5247 |
| 6 | `wfarsenal/record_keys.py:16(record_key)` | 0.0248 | `wfarsenal/optimizer.py:5715(_optimize_archetype)` | 0.3623 |
| 7 | `<built-in method wfarsenal.wfcore.trajectory_health_batch>` | 0.0217 | `wfarsenal/search_session.py:119(run)` | 0.3404 |
| 8 | `wfarsenal/optimizer.py:1758(forma_plan)` | 0.0199 | `wfarsenal/optimizer.py:2795(_optimize)` | 0.3379 |
| 9 | `wfarsenal/evaluation_cache.py:24(_card)` | 0.0173 | `wfarsenal/optimizer.py:2057(_score)` | 0.2342 |
| 10 | `wfarsenal/optimizer.py:2655(_evaluate)` | 0.0167 | `wfarsenal/optimizer.py:6056(_search_actual)` | 0.2192 |
| 11 | `wfarsenal/mods_native.py:266(apply)` | 0.0156 | `wfarsenal/optimizer.py:2011(_event_window)` | 0.1581 |
| 12 | `wfarsenal/manual_combat.py:253(prepare)` | 0.0132 | `wfarsenal/search_combat.py:100(window)` | 0.1579 |
| 13 | `wfarsenal/optimizer.py:1680(_effects)` | 0.0117 | `wfarsenal/optimizer.py:2352(refine_final_build)` | 0.1540 |
| 14 | `wfarsenal/simulator.py:690(_set_key)` | 0.0106 | `wfarsenal/native_replay.py:486(trajectory)` | 0.1327 |
| 15 | `<built-in method wfarsenal.wfcore.sustained_status>` | 0.0096 | `wfarsenal/combat_replay.py:73(replay)` | 0.1052 |

#### `trait-matrix` 前 15 项

| 排位 | 按 `tottime` 的函数 | 自身 s | 按 `cumtime` 的函数 | 累计 s |
| ---: | --- | ---: | --- | ---: |
| 1 | `<method 'get' of 'dict' objects>` | 0.2687 | `app.py:4817(trait_matrix_payload)` | 2.8265 |
| 2 | `wfarsenal/manual_combat.py:253(prepare)` | 0.1748 | `wfarsenal/optimizer.py:2655(_evaluate)` | 2.7203 |
| 3 | `<built-in method wfarsenal.wfcore.trajectory>` | 0.1632 | `wfarsenal/search_session.py:119(run)` | 2.6810 |
| 4 | `<built-in method wfarsenal.wfcore.apply_mods>` | 0.1514 | `wfarsenal/optimizer.py:2795(_optimize)` | 2.6805 |
| 5 | `wfarsenal/mods_native.py:70(_build)` | 0.1389 | `app.py:4127(build_weapon_payload)` | 2.4711 |
| 6 | `<built-in method _pickle.dumps>` | 0.0837 | `app.py:4546(search)` | 2.4462 |
| 7 | `标准库/dataclasses.py:1540(replace)` | 0.0705 | `wfarsenal/optimizer.py:2208(optimize_for_element)` | 2.3210 |
| 8 | `wfarsenal/manual_combat.py:817(resolve)` | 0.0586 | `wfarsenal/optimizer.py:2057(_score)` | 1.4531 |
| 9 | `wfarsenal/mods_native.py:266(apply)` | 0.0574 | `wfarsenal/optimizer.py:2011(_event_window)` | 1.4283 |
| 10 | `wfarsenal/manual_combat.py:281(<genexpr>)` | 0.0538 | `wfarsenal/search_combat.py:100(window)` | 1.4246 |
| 11 | `wfarsenal/damage.py:2379(_apply_heavy_components)` | 0.0534 | `wfarsenal/damage.py:1581(apply_mods)` | 0.9107 |
| 12 | `wfarsenal/record_keys.py:16(record_key)` | 0.0527 | `wfarsenal/damage.py:1751(_apply_mods_spec_py)` | 0.8719 |
| 13 | `wfarsenal/native_replay.py:486(trajectory)` | 0.0468 | `wfarsenal/damage.py:2379(_apply_heavy_components)` | 0.8329 |
| 14 | `wfarsenal/runtime_adapters.py:810(verified_incarnon_condition)` | 0.0460 | `wfarsenal/mods_native.py:266(apply)` | 0.7350 |
| 15 | `<built-in method builtins.getattr>` | 0.0409 | `wfarsenal/manual_combat.py:253(prepare)` | 0.6701 |

#### `simulate` 前 15 项

| 排位 | 按 `tottime` 的函数 | 自身 s | 按 `cumtime` 的函数 | 累计 s |
| ---: | --- | ---: | --- | ---: |
| 1 | `<method 'get' of 'dict' objects>` | 0.0199 | `app.py:6611(simulate_manual_payload)` | 0.2845 |
| 2 | `wfarsenal/simulator.py:690(_set_key)` | 0.0173 | `wfarsenal/combat_replay.py:73(replay)` | 0.1668 |
| 3 | `wfarsenal/combat_replay.py:234(_reference_trajectory)` | 0.0109 | `wfarsenal/combat_replay.py:184(_trajectory)` | 0.1668 |
| 4 | `wfarsenal/combat_replay.py:1380(add_procs)` | 0.0106 | `wfarsenal/combat_replay.py:234(_reference_trajectory)` | 0.1667 |
| 5 | `wfarsenal/proc_probability.py:16(no_status_probability)` | 0.0088 | `wfarsenal/simulator.py:791(simulate)` | 0.1031 |
| 6 | `wfarsenal/simulator.py:237(<genexpr>)` | 0.0086 | `wfarsenal/combat_replay.py:1380(add_procs)` | 0.0727 |
| 7 | `wfarsenal/combat_replay.py:575(clean)` | 0.0079 | `wfarsenal/combat_replay.py:929(radial_hit)` | 0.0513 |
| 8 | `<built-in method builtins.max>` | 0.0074 | `wfarsenal/simulator.py:695(_set_shells)` | 0.0380 |
| 9 | `<method 'casefold' of 'str' objects>` | 0.0071 | `wfarsenal/proc_probability.py:16(no_status_probability)` | 0.0310 |
| 10 | `wfarsenal/simulator.py:695(_set_shells)` | 0.0069 | `wfarsenal/simulator.py:690(_set_key)` | 0.0286 |
| 11 | `wfarsenal/proc_probability.py:241(add)` | 0.0063 | `wfarsenal/simulator.py:218(resolve_record)` | 0.0230 |
| 12 | `<built-in method builtins.min>` | 0.0063 | `wfarsenal/combat_replay.py:957(take)` | 0.0229 |
| 13 | `<built-in method builtins.sum>` | 0.0049 | `wfarsenal/combat_replay.py:1264(packet)` | 0.0205 |
| 14 | `wfarsenal/combat_replay.py:636(signal)` | 0.0047 | `<method 'get' of 'dict' objects>` | 0.0199 |
| 15 | `wfarsenal/simulator.py:218(resolve_record)` | 0.0040 | `<built-in method builtins.sum>` | 0.0189 |

### 后续阶段优先级

1. **Phase 1 优先迁移候选评估及其输入准备。** 重击震地矩阵的剖析墙钟为 2.827 秒，其中 `_evaluate` 的累计时间为 2.720 秒，共进行了 2899 次原生回放。`manual_combat.prepare` 和 `mods_native._build` 分别用了 0.175 和 0.139 秒自身时间，矩阵约 88.68% 的自身时间在 `wfcore` 可识别入口之外。先让 Rust 复用已解析的武器、目标和配装输入，再逐项迁移配卡候选评估及词条试算。保持候选、槽位、平手顺序和分数一致。
2. **Phase 2 优先对齐 Rust HTTP 的完整回放与响应。** 伯斯顿模拟的 Python `_reference_trajectory` 链累计占 0.167 秒，约为剖析墙钟的 59%。其原生回放计数为 0。先对齐完整时间线、叠层和伤害分项，再用同一黄金夹具验证六个端点。配装的 `wfcore.trajectory` 自身用时约 0.297 秒，待跨语言数据准备收敛后再定位原生循环优化空间。
3. **Phase 3 以真实进程指标安排发布优化。** 切换发布入口前，在同一宿主机对 Python 与 Rust 各自执行健康启动、首轮负载和重复请求采样。用实测结果确定目录序列化、数据加载、缓存和内存优化的顺序，并复测客户端依赖的指纹与版本。
