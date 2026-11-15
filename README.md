# Warframe Damage Engine

面向同机插件的 Warframe 配装面板与战斗模拟服务。计算核心基于 [WFSim](https://github.com/magenie33/wfsim)，固定上游源码与数据版本，记录本项目对游戏机制的修订。

## 构建和运行

使用 Git、Bash、Rust 1.97 或更新版本，以及 Python 3.10 或更新版本。`UPSTREAM` 依次记录上游提交和对应的 Git 树。构建时在 `.build/wfsim` 取出锁定源码，应用 `violet-hcet.patch`，再按 `Cargo.lock` 编译。

```sh
bash build.sh
python3 server.py
```

服务监听 `127.0.0.1:8788`，可用 `--port` 调整端口。设置 `WARFRAME_ENGINE_TOKEN` 后，计算请求需提供相同的 Bearer 令牌。`GET /healthz` 返回引擎版本、数据修订和源码地址。

`POST /api/panel` 计算军械库面板，`POST /api/simulate` 执行战斗模拟。`POST /api/riven` 提供紫卡数值计算，`POST /api/candidates` 读取合法候选项。请求沿用冻结版本的结构化字段。

```sh
curl -sS http://127.0.0.1:8788/api/panel \
  -H 'Content-Type: application/json' \
  -d '{"weapon":"tenet_glaxion","valence_element":"toxin","valence_bonus":0.56,"mods":["serration"],"wielder":{"frame":"wukong_prime","shards":[{"shard":"violet_archon_shard","effect":"primary_electricity_damage","tauforged":true}]}}'
```

本地也可直接执行 `target/release/warframe-damage-engine`。标准输入每行接收一个带 `path` 和 `body` 的 JSON 对象，每行返回 `response` 和原样回显的可选 `id`。

## 紫晶电击的合成顺序

来源为主武电击紫晶时，电击和武器自带、效价单元素一同按火、冰、电、毒排序。同属性 MOD 能把该元素提前到对应槽位。毒效价信条·冷冻光束步枪在无元素 MOD 时，自带冰冻与紫晶电击配成**磁力**，效价**毒素**保留。普通与 Tau 紫晶均走这条规则。这个组合的**实机面板待复核**。 [Wiki 合成说明](https://wiki.warframe.com/w/Violet_Archon_Shard#Primary_Electricity_Damage) · [U44 玩家报告](https://forums.warframe.com/topic/1529294-elements-on-the-weapons-combine-in-an-inconsistent-manner/)

用 `python3 -m unittest -v test_engine` 检查同一毒效价的零石、普通紫晶、Tau 紫晶和 Tau 紫晶配火卡面板，也检查版本、鉴权与战斗模拟入口。

## 许可和来源

代码采用 [AGPL-3.0-or-later](LICENSE)。对应源码包括 `UPSTREAM` 指定的 [WFSim 源码](https://github.com/magenie33/wfsim/tree/cddfac4b10c71fd162be0ae7be32778aa5d2fd06)、此仓的补丁和构建文件。游戏数据的上游来源为 [Warframe Wiki](https://wiki.warframe.com/)。构建在本地读取上游资源，数据、测量记录和游戏素材沿用各自条款。 [WFSim 数据许可说明](https://github.com/magenie33/wfsim/blob/cddfac4b10c71fd162be0ae7be32778aa5d2fd06/LICENSE-DATA.md) · [WFSim 标识政策](https://github.com/magenie33/wfsim/blob/cddfac4b10c71fd162be0ae7be32778aa5d2fd06/TRADEMARK.md)
