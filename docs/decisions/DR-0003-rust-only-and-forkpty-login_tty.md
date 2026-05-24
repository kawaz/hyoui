# DR-0003: Rust 一本化 (MoonBit 却下) と forkpty + login_tty 採用

- Status: Active
- Date: 2026-05-25
- Related: DR-0001 (bg/fg ジョブ制御の 2 軸設計), DR-0002 (プロジェクト名), docs/journal/2026-05-25-rust-rebuild.md

## Context

poc 段階の実装は **MoonBit + Rust FFI 二層構成** だった (`ffi/` 1146 行 + `lib/agent/` 988 行)。
PTY ラッパー + 観察 + 外部制御という性格上、コードの大部分が syscall を直接叩く層であり、
純粋ロジック層 (シェル外プロトコル、状態機械) は数百行に留まる。

DR-0001 で確立した「親 fg ⇒ 子 fg」invariant は、SIGTSTP/SIGCONT、PTY の制御端末獲得、
tcsetpgrp、posix_spawn のシグナル属性などを**精密に**順序制御する必要がある。
ランタイムがシグナルに介入する処理系では invariant の維持が困難になる。

## Decision

- **実装言語: Rust 一本化**。MoonBit 層を全廃
- **syscall 抽象: nix crate 主体 + libc 直** (nix 未提供の `posix_spawn`/`login_tty` 等のみ libc)
- **子プロセス起動: `forkpty(3)` + `login_tty(3)`** で PTY 制御端末を確実に獲得
- **unsafe は 2 ファイルに封じ込め**: `crates/hyoui/src/sys/raw.rs` (raw mode / winsize ioctl)
  と `crates/hyoui/src/sys/signal.rs` (sigaction)。他は `unsafe_op_in_unsafe_fn = "deny"` で網
- bin crate (`crates/hyoui-cli`) は `unsafe_code = "forbid"` で物理的に unsafe を禁止

## Rejected alternatives

### MoonBit + Rust FFI 二層
- ロジック層が薄く、二層維持の税金 (FFI binding、型変換、ビルドシステム二重化) に見合わない
- syscall 比率が高いため Rust 側に集約した方が単純
- MoonBit ランタイムの PTY/シグナル系での実績が薄く、DR-0001 invariant の検証コストが高い

### Go
- ランタイムが SIGCHLD・SIGURG を含む多くのシグナルを掴み、`signal.Notify` チャネル経由でしか
  扱えない。DR-0001 の精密な順序制御 (例: SIGTSTP を子に送る直前/直後で tcsetpgrp する) と
  チャネル経由の非同期通知が噛み合わない
- PTY raw mode 中の GC stop-the-world が入力レイテンシに乗る可能性
- forkpty/login_tty は cgo になり、二層構成の負担は MoonBit と同根

### posix_spawn + POSIX_SPAWN_SETSID + 後付け TIOCSCTTY
- 子側で setsid 直後に PTY slave を `open()` すれば自動 ctty 化されるのが POSIX 規定だが、
  macOS の動作実績にぶれがあり (バージョン依存)、フォールバックで `TIOCSCTTY` ioctl を明示する
  必要が出てくる
- `login_tty(3)` がこの一連の手順 (setsid + ctty 化 + stdin/out/err 接続) をアトミックに
  まとめてくれるため、自前で組むより事故が少ない
- `posix_spawn` も依然として「子側で `login_tty` 相当を `posix_spawn_file_actions` 経由で
  組む」必要があり、forkpty + login_tty の方が短くバグりにくい

## Consequences

### 廃止 / 保全
- 既存 `ffi/` 1146 行と `lib/agent/` 988 行は **bootstrap workspace に保全** (参考のためだけ
  残す、`main` workspace からは見えない)
- MoonBit 関連 toolchain (`moon.mod.json`, `.mooncakes` 等) は `main` から削除済み

### Cargo workspace 構成
```
crates/hyoui      (lib)   sys + observer + protocol + cli + agent
crates/hyoui-cli  (bin)   hyoui CLI 実体 (unsafe_code = "forbid")
```

### unsafe 封じ込めの自動検証
- Taskfile.pkl の `lint:unsafe` task と CI の `unsafe-gate` job で grep ベースの
  whitelist check。`sys/raw.rs` と `sys/signal.rs` 以外で `unsafe { ... }` / `unsafe fn` 等が
  現れたら fail

### テスト
- `cargo test --workspace` で 97 件全 pass
- ctty 検証: `tcgetpgrp(master_fd) == child_pid` で「子が PTY を制御端末として獲得した」を確認
- 子の suspend 検出: 当初 `SIGTSTP` を子に送って `WaitStatus::Stopped` を観測する設計だったが、
  cargo test 環境 (libtest harness) では SIGTSTP が SIG_IGN になっており子に継承されて止まらない
  現象を確認。**`SIGSTOP` (catchable でない・継承で必ず停止)** に切り替えて安定化
