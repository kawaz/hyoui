# hyoui justfile
#
# Canonical task runner. VCS 操作 (clean check / diff / commit / push) と翻訳ペア
# 鮮度チェックは `bump-semver vcs` サブコマンドに委譲して jj/git 透過化。
# kawaz/bump-semver・kawaz/claude-gh-monitor の justfile と同じ流儀。
#
# Taskfile.pkl は残置 (pkf 運用は廃止だが file としては残す)。
#
# 宣言順は意図的 = 利用頻度の高い recipe を上に並べ、`just --list` / `default` で目立たせる。

set shell := ["bash", "-euo", "pipefail", "-c"]

set script-interpreter := ["bash", "-euo", "pipefail"]

set positional-arguments

# default behaviour: alias for `list`
default: list

# show the recipe list
list:
    @just --list --unsorted

# ---------- atomic (lint / test / build) ----------

# cargo fmt --check + cargo clippy
[private]
lint-rust:
    cargo fmt --check
    cargo clippy --workspace --all-targets -- -D warnings

# unsafe 封じ込め gate: unsafe は sys/raw.rs / sys/signal.rs / sys/env.rs のみに限定。
# 他に漏れたら fail。grep がマッチ無しでも exit 0 扱いとする (= 漏れなし = 成功)。
[private]
[script]
lint-unsafe:
    leaked=$(grep -rnE '\bunsafe[[:space:]]+(fn|impl|trait|extern|\{)' crates/hyoui/src --include='*.rs' \
      | grep -v 'src/sys/raw.rs\|src/sys/signal.rs\|src/sys/env.rs' || true)
    if [ -n "$leaked" ]; then
      echo "ERROR: unsafe leaked outside whitelisted files (sys/raw.rs, sys/signal.rs, sys/env.rs):" >&2
      echo "$leaked" >&2
      exit 1
    fi

# lint-rust + lint-unsafe
lint: lint-rust lint-unsafe

# cargo test --workspace (ARGS で追加引数を渡せる、例: `just test -- --nocapture`)
[script]
test *ARGS: lint
    cargo test --workspace --no-fail-fast "$@"

# cargo build --release --workspace
build: lint
    cargo build --release --workspace

# build then run the local binary, forwarding all args
[script]
run *ARGS: build
    ./target/release/hyoui "$@"

# lint + test + build (CI entry point)
ci: lint test build

# ---------- gates (push の内部、利用者が直接叩くことほぼなし) ----------

# working copy が clean (= @ が empty change) であることを検証
[private]
ensure-clean:
    bump-semver vcs is clean

# 翻訳ペア (README-ja.md ↔ README.md, docs/DESIGN-ja.md ↔ docs/DESIGN.md 等) の鮮度チェック。
# FROM = ja 版 (source、日本語で先に書く)、TO = $1/$2.md (en 版、derived)。
# en 版が ja 版より古いと fail。canonical (bump-semver / claude-gh-monitor) と同じ glob 規約。
[private]
check-outdated-translations: ensure-clean
    bump-semver vcs outdated 'glob:**/*-ja.md' '$1/$2.md'

# crates/ の src が main@origin から変わっているのに Cargo.toml の version が
# 上がっていなければ fail。
# bump-semver vcs diff の exit code:
#   0 = 対象 path に変更なし → bump 不要
#   1 = 変更あり → version bump 済みかチェックに進む
#   その他 = VCS error (main@origin 未 track 等)
[private]
check-version-bumped:
    #!/usr/bin/env bash
    set -euo pipefail
    rc=0
    bump-semver vcs diff -q main@origin -- crates/hyoui/src/ crates/hyoui-cli/src/ || rc=$?
    case "$rc" in
      0) exit 0 ;;
      1) ;;
      *) echo "ERROR: bump-semver vcs diff failed (rc=$rc). main@origin が track されていない可能性。先に 'jj git fetch' / 'git fetch' を試してください" >&2; exit 1 ;;
    esac
    bump-semver compare gt Cargo.toml vcs:main@origin:Cargo.toml --no-hint && exit 0
    echo 'ERROR: crates/ の src が変わっているが Cargo.toml version 未 bump。"just bump-version" を実行してください' >&2
    exit 1

# ---------- release flow ----------

# Cargo.toml workspace.package.version を bump (default: patch) し、
# workspace 内 path 依存の version 制約も同期して Release commit を作る。
[script]
bump-version level="patch": ensure-clean
    new_version=$(bump-semver "$1" Cargo.toml --write --no-hint)
    # workspace 内 path 依存の version 制約も同期 (例: hyoui-cli が hyoui を参照)。
    # `version = "X.Y"` 形式 (semver major.minor の short form) のみ更新する。
    # `version = "X.Y.Z"` 等の full pin や `>=X.Y` は変えない (= 意図ある制約は保持)。
    new_minor=$(printf '%s' "$new_version" | perl -ne 'print "$1.$2" if /^(\d+)\.(\d+)\.\d+/')
    for f in crates/*/Cargo.toml; do
      perl -i -pe 'BEGIN{$v=shift @ARGV} s/(path\s*=\s*"\.\.\/[^"]+"\s*,\s*version\s*=\s*")\d+\.\d+(")/$1$v$2/g' \
        "$new_minor" "$f"
    done
    # cargo check で Cargo.lock を再生成 (workspace 全体)
    cargo check --workspace --offline >/dev/null 2>&1 || cargo check --workspace >/dev/null
    # Cargo.toml + crates/*/Cargo.toml + Cargo.lock を一括 commit (--staged = 全 dirty)
    bump-semver vcs commit -m "Release v${new_version}" --staged
    echo "Version: -> ${new_version}"

# push to origin/main with gates
push: ensure-clean ci check-outdated-translations check-version-bumped
    bump-semver vcs push --branch main --jj-bookmark-auto-advance

# ---------- utility ----------

# 現在の version を表示 (Cargo.toml workspace.package.version)
version:
    @bump-semver get Cargo.toml --no-hint
