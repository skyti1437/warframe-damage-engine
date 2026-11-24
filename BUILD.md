# 构建与复验

本目录包含 `portable-535ed49f5a51149d` 运行包对应的 Python、Rust、锁文件和构建入口。采用 CPython 3.12 与 Rust。默认单工位服务绑定在本机。

```sh
python tools/build_native.py
```

从本目录 `downloads/` 内任一程序包读取 `manifest.json`。将 `upstreamFiles` 所列文件按原路径放入本目录，逐个核对 `files` 中的 SHA-256。插件后台可以自动完成该步骤并使用已编译运行包。 [固定上游提交](https://github.com/superexboom/warframe-weapon-loadout-build/tree/d62e4ae56387e241d512dcf9176f3bfd3d426de7)

程序包内的 `data/warframe_build_data.json` 用于战甲目录和引擎指纹校验。

```sh
python launch.py --probe
python launch.py --port 8765
```

探针返回引擎指纹 `535ed49f5a51149d`、数据版本 `2026.08.13.13.24` 与接口版本 3。 [项目和第三方代码来源](THIRD_PARTY_NOTICES.md)
