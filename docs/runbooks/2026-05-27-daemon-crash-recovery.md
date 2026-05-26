# daemon panic/abort からの復旧手順

> Status: Active
> Date: 2026-05-27
> Related: [[R5-H11]] (lock_token panic 解消)、[[R5-H12]] (core dump 抑止)

## 症状

- daemon プロセスが消失している (= `ps -ef | grep hyoui` でヒットなし)
- `hyoui list` に出てくるが `hyoui status <session>` が `ECONNREFUSED`
- panic = abort モードで build されているため、core dump は通常 **抑止**
  されている (= `setrlimit(RLIMIT_CORE, 0)` を起動時に設定)
- attached client は接続が切れた状態で残っている (= client 側の read が
  EOF/ECONNRESET を返す)

## 切り分け

1. **stale socket かどうか**を `hyoui list --prune-stale` で確認
   (詳細は [[2026-05-27-stale-socket-detection]] runbook)
2. **どのように落ちたか**を OS ログから推測:
   ```bash
   # macOS
   log show --last 1h --predicate 'process == "hyoui"' --info --debug
   # Linux
   journalctl --since "1 hour ago" | grep -i hyoui
   dmesg | grep -i 'hyoui\|oom'
   ```
   - SIGKILL: OOM-killer、ユーザ `kill -9`、`launchd`/`systemd` の停止
   - SIGABRT: panic → abort パス (= R5-H11 で `expect` を排除済だが、
     新規 panic 経路が混入した可能性)
   - SIGSEGV: unsafe 周辺の bug (= sys/pty/socket/cli モジュール疑い)
3. **abort の root cause を再現** (= core dump 必要時のみ):
   ```bash
   # 開発時のみ。本番では core 抑止を維持する (= R5-H12 セキュリティ要件)
   HYOUI_ALLOW_CORE=1 hyoui run <session> <command>
   # その後 panic 経路を踏むと core が吐かれる
   ulimit -c unlimited   # 必要なら追加
   ```
   - `HYOUI_ALLOW_CORE=1` は **debug 用 opt-in**。本番では絶対に設定しない
     (= memory 内の lock token / `HYOUI_LOCK_TOKEN` env が disk 漏洩する)
4. backtrace を確認: panic = abort なので stack trace は得られにくい。
   `RUST_BACKTRACE=1 HYOUI_ALLOW_CORE=1` でフォアグラウンド実行
   (= `--no-daemonize` 相当の経路がある場合) し、stderr に出させるのが
   最速

## 対処

1. **stale socket を掃除**:
   ```bash
   hyoui list --prune-stale
   ```
2. **新 daemon 起動**: 通常通り `hyoui run <session> <command>`
   - 落ちた session の child は親 daemon と一緒に死んでいる (= R5-H7 で
     `killpg` 化済、孫プロセスも道連れ)
3. **client 側の再 attach**: 切断された client は手動で再 attach
4. **再発防止のためのログ収集** (= R5-SRE-C1 対応後):
   ```bash
   tail -F "$XDG_STATE_HOME/hyoui/<session>.log"   # 次回 panic 時用に張る
   ```
5. **panic が頻発する場合**: ROADMAP の v0.2.0 で structured logging 導入
   待ち。それまでは `HYOUI_ALLOW_CORE=1` を **隔離環境のみ**で使って再現

## 予防

- **panic = abort は維持**する (= R5-H12 で決定済の security trade-off):
  - core dump 抑止 → memory 内 secret 漏洩防止
  - 代償として stack trace が取りにくい
  - debug 必要時は `HYOUI_ALLOW_CORE=1` で隔離環境のみ opt-in
- panic 経路を追加しない:
  - `unwrap()` / `expect()` は **path として残さない** (= R5-H11 解消の方針)
  - 新規コードは `Result` で propagate、daemon main loop で集約 log
- daemon 起動時の env を厳格管理:
  - 本番では `HYOUI_ALLOW_CORE` を **絶対に export しない**
  - `HYOUI_LOCK_TOKEN` も同様、必要 process にだけ inherit させる
- OOM 対策: `client_buffer_bytes × MAX_CLIENTS_PER_DAEMON` の上限が RSS を
  上回らないことを cap 値で保証 ([[2026-05-27-backpressure-disconnect]] 参照)

## 関連

- [[R5-H11]] — `generate_lock_token` の `expect` を排除した経緯
- [[R5-H12]] — core dump 抑止導入 (= `setrlimit(RLIMIT_CORE, 0)`、
  `HYOUI_ALLOW_CORE=1` で opt-in)
- [[R5-H7]] — `killpg` 化により daemon 死亡 = 子孫プロセス全死亡
- [[R5-SRE-C1]] — 構造化ログ基盤の整備 (panic stack を残すための前提)
- `crates/hyoui/src/daemon/session.rs:148` — `HYOUI_ALLOW_CORE` parse
- `crates/hyoui/src/daemon/session.rs:178` — `setrlimit(RLIMIT_CORE, 0)` 実装
- [[2026-05-27-stale-socket-detection]] — socket 残骸の掃除
