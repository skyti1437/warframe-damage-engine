#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "$0")"

upstream_url=https://github.com/magenie33/wfsim.git
source_dir=.build/wfsim
patch_file="$PWD/violet-hcet.patch"
{
    read -r revision
    read -r tree
} < UPSTREAM

if [[ ! -d "$source_dir/.git" ]]; then
    mkdir -p "$source_dir"
    git -C "$source_dir" init --quiet
    git -C "$source_dir" remote add origin "$upstream_url"
    git -C "$source_dir" sparse-checkout set --cone \
        engine optimizer webapi tools/gas-macro data cli web wasm native
fi
if ! git -C "$source_dir" cat-file -e "$revision^{commit}" 2>/dev/null; then
    git -C "$source_dir" fetch --no-tags --depth=1 --filter=blob:none origin "$revision"
fi
if [[ $(git -C "$source_dir" rev-parse "$revision^{tree}") != "$tree" ]]; then
    echo '上游源码树与锁定版本不符' >&2
    exit 1
fi

# 这个目录由构建生成，先复原冻结源码，再应用当前补丁。
git -C "$source_dir" checkout --quiet --detach --force "$revision"
git -C "$source_dir" apply --check "$patch_file"
git -C "$source_dir" apply "$patch_file"
cargo build --release --locked "$@"
