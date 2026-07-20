# stale socket 検出と削除

> Status: Active
> Date: 2026-05-27
> Related: [[DR-0006]] (CLI 地盤ルール、socket 配置)、[[R5-H3]] (backlog)、[[R5-SRE-C3]]

## 症状

- `hyoui list` に出てくる socket file の中に、`hyoui status <name>` で
  `Connection refused` / `ECONNREFUSED` を返すものがある
- 過去に daemon が `SIGKILL` を受けた、または `panic = abort` で abort した
  履歴がある (OS のジョブ kill / OOM-killer / `kill -9` 等)
- ホスト再起動後で `${XDG_RUNTIME_DIR}` ベースなら消えているはずだが、
  `${XDG_STATE_HOME:-$HOME/.local/state}/hyoui` 配下に残骸が残っている
- `hyoui list --prune-stale` 未対応の旧版 (< v0.1.7) では「list に出るが
  status は失敗」が見分けられない

## 切り分け

1. `hyoui list --prune-stale=false`(デフォルト) で **socket file の存在だけ**
   を列挙する。live/stale 列があれば確認:
   - `live` 列: connect + handshake 成功 (= daemon が生きている)
   - `stale` 列: connect 失敗 (= ECONNREFUSED や timeout)
2. stale が確認できたら、対応する PID が本当に死んでいるかを別経路でも
   裏取りする (= socket 名と process の対応がローカル設計依存なので、
   `ps -ef | grep hyoui` または `lsof -U` で socket file を握っている
   プロセスの不在を確認):
   ```bash
   lsof -U 2>/dev/null | grep '<session-name>.sock'
   # 何も出なければ holder 不在 = stale 確定
   ```
3. `hyoui list --prune-stale` 未対応バージョンの場合は手動 `unlink`:
   ```bash
   rm -- "$XDG_RUNTIME_DIR/hyoui/<session>.sock"  # or ${XDG_STATE_HOME:-$HOME/.local/state}/hyoui/<session>.sock
   ```

## 対処

1. **推奨**: `hyoui list --prune-stale` で一括掃除
   - 内部で connect 試行 → ECONNREFUSED 等を確定とみなして `unlink(2)`
   - live と判定された socket は触らない (= 並走 daemon を誤殺しない)
2. **個別削除** (= live が並走していて全削除が怖い場合):
   ```bash
   # まず stale を listing で特定
   hyoui list
   # 名前で個別に消す
   rm -- "$(hyoui list | awk '$2 == "stale" {print $3}')"
   ```
3. **再起動経路**: stale 掃除 → `hyoui run <session> <command>` で再生成

## 予防

- daemon が落ちる原因を残さない:
  - `panic = abort` の現行ビルドでは、daemon の Drop chain が走らないため
    `UnixSock::Drop` の `unlink` が呼ばれない。これは仕様 (= R5-H12 で core
    dump 抑止優先) なので、stale 化を覚悟して `--prune-stale` の運用を組む
- CI 等で並列に daemon 起動 → 殺すパターンでは、ジョブ末尾で必ず
  `hyoui list --prune-stale` を呼んで掃除する
- 監視: `hyoui list` の出力を定期的にスナップショットして、stale 件数が
  閾値超え → アラート (= 障害の predictor として有効)

## 関連

- [[DR-0006]] §socket-placement — `${XDG_RUNTIME_DIR}/hyoui/<session>.sock`
  または `${XDG_STATE_HOME:-$HOME/.local/state}/hyoui` フォールバック
- [[R5-H3]] — backlog の解消経緯 (= `list` 改修で live/stale 列追加 +
  `--prune-stale` flag)
- [[R5-H12]] — `panic = abort` を維持する判断 (= 引き換えに stale socket
  確定、unlink は手動 or `--prune-stale` で対応)
- `crates/hyoui/src/sys/socket.rs:144` — `UnixSock::Drop` 実装 (graceful exit
  時のみ unlink される)
- `crates/hyoui/src/cli.rs:336` — `--prune-stale` parse
