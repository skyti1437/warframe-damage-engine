# 构建与复验

本目录包含 `portable-85ec9cdb3461c01c` 运行包对应的 Python、Rust、锁文件和构建入口。采用 CPython 3.12 与 Rust。默认单工位服务绑定在本机。

```sh
python tools/build_native.py
```

从本目录 `downloads/` 内任一程序包读取 `manifest.json`。将 `upstreamFiles` 所列文件从 [固定上游提交](https://github.com/superexboom/warframe-weapon-loadout-build/tree/44096ddfbc50470656f5b13b6a0b45e9c24d93c7) 按原路径放入本目录，逐个核对 `files` 中的 SHA-256。插件后台可以自动完成该步骤并使用已编译运行包。

```sh
python launch.py --probe
python launch.py --port 8765
```

探针返回引擎指纹 `85ec9cdb3461c01c`、数据版本 `2026.08.13.13.24` 与接口版本 3。此对应源码快照于 2026-11-20 整理。项目和第三方代码的来源见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
