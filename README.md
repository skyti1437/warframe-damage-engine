# SDJKBOT 完整伤害模拟引擎

AstrBot 插件的本机完整伤害模拟服务，提供固定配装复算、紫卡评级和配卡搜索。当前程序版本为 `portable-535ed49f5a51149d`，数据标记为 `2026.08.13.13.24`。

## 安装

在插件运维面板的「伤害引擎」页选择「完整模拟」，填写空闲的本机端口（例如 8765）并保存，再点「下载并部署」。安装器按宿主平台选择包，校验程序包及 45 个数据文件，启动后核对引擎指纹。支持 CPython 3.12 的 Linux x86_64 和 macOS arm64。

程序包与 SHA-256 见 [release.json](release.json)。维护者可以用 [BUILD.md](BUILD.md) 从此版本的 Python 和 Rust 源码构建。插件的内置估算模式使用插件本地资料。

## 数据与许可证

- 本版本移植自 warframe-weapon-loadout-build 的固定提交，保留原作者及 WFSim 贡献者的声明。本机服务启动使用一个求解工位，加入配装候选复用和预算分配，并提供六组预计算工况。Python、Rust 及本机服务入口采用 AGPL-3.0-only。 [固定上游提交](https://github.com/superexboom/warframe-weapon-loadout-build/tree/d62e4ae56387e241d512dcf9176f3bfd3d426de7) [许可证](LICENSE) [第三方声明](THIRD_PARTY_NOTICES.md)
- 独立程序包包括服务、原生模块、预计算配置和得分。安装器从上述提交直接读取 `data/` 下的锁定文件。客户端导出表、官方文字和 Wiki 资料按各自来源及许可标注。Wiki 提供部分武器修正值和架势资料。 [上游数据与许可说明](https://github.com/superexboom/warframe-weapon-loadout-build/blob/d62e4ae56387e241d512dcf9176f3bfd3d426de7/THIRD_PARTY_NOTICES.md)
- 旧 WFSim 版和对应构建文件见 [固定提交 2419c5c](https://github.com/skyti1437/warframe-damage-engine/tree/2419c5c76a83aae34428fafc5da2eee21ec16093)。

本项目为非官方社区工具。Warframe 商标属于 Digital Extremes Ltd.
