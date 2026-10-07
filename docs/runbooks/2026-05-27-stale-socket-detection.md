# stale socket 検出と削除

> Status: Active
> Date: 2026-05-27
> Related: [[DR-0006]] (CLI 地盤ルール、socket 配置)、[[R5-H3]] (backlog)、[[R5-SRE-C3]]

## 症状

- `hyoui list` に出てくる socket file の中に、`hyoui status <name>` で
  `Connection refused` / `ECONNREFUSED` を返すものがある
- 過去に daemon が `SIGKILL` を受けた、または `panic = abort` で abort した
  履歴がある (OS のジョブ kill / OOM-killer / `kill -9` 等)
- `<状態の root>/sessions/` (= `$HYOUI_STATE_DIR` か `$XDG_STATE_HOME/hyoui` か
  `$HOME/.local/state/hyoui` の下、DR-0041) に残骸が残っている
- 同じ id で `hyoui run --session-id=<id>` が「socket が既にある」で起動しない
  (DR-0041 決定 3。run は生死を判定しないので、残骸の片付けはこの runbook の経路で行う)
- `hyoui list --prune-stale` 未対応の旧版 (< v0.1.7) では「list に出るが
  status は失敗」が見分けられない

## 切り分け

1. `hyoui list` で状態を見る。STATUS 列 (SESSION が `$1`、STATUS が `$2`、PID が `$3`) は次のどれか:
   - `live` / `stopped`: daemon が応答した (stopped は子が ^Z / SIGSTOP で止まっている)
   - `no-response`: 接続はできたが 5 秒以内に応答が無い、または daemon が lock を持ったまま接続を受け付けられない (backlog 飽和)。daemon は生きているので消さない
   - `stale`: 接続が拒否され、lock (`<id>.lock`) が無いので daemon の生死を判断できない。`hyoui list` は消さずに表示だけする
   - `error`: 接続後の明示的な失敗 (handshake の拒否等)
   - 一覧から消えたもの: 接続が拒否され、lock が在って誰も持っていなかった socket。`hyoui list` が socket と lock を消した (= daemon は死んでいた)
2. `stale` の socket は、daemon が本当に居ないかを別の経路で確かめる (= socket を開いている process が居ないか):
   ```bash
   lsof -U 2>/dev/null | grep '<id>.sock'
   # 何も出なければ socket を持つ process は居ない
   ```

## 対処

1. **lock が残っている残骸**: `hyoui list` を 1 回打つ。接続を拒否し、lock を誰も持っていない socket を、socket と lock ごと消す。lock を持っている (= daemon が生きている) socket と、lock の無い socket には触らない
2. **`stale` と出る socket (lock が無い)**: 手順「切り分け 2」で daemon が居ないことを確かめてから、手で消す:
   ```bash
   hyoui list | awk '$2 == "stale" {print $1}'          # stale の session id
   rm -- "<状態の root>/sessions/<id>.sock"
   ```
   lock の無い socket を自動で消さないのは、lock を持たない生きた daemon の socket が、backlog 飽和の瞬間に接続を拒否することがあるため (死んだと誤って消すと、その session に二度と届かない)
3. **同じ id で起動し直す**: 片付けた後に `hyoui run --session-id=<id> -- <cmd>`。動いている session を止めてすぐ同じ id を使う時は `hyoui kill --wait <id>` (daemon の終了まで待つ) を使う

## 予防

- `panic = abort` の build では daemon の Drop が走らず、`UnixSock::Drop` の unlink が呼ばれない (R5-H12 で core dump の抑止を優先した仕様)。この時も lock file は残るので、次の `hyoui list` が片付ける
- CI 等で daemon を並列に起こして殺す時は、ジョブの末尾で `hyoui list` を打って片付ける
- 監視: `hyoui list` の出力を定期的に見て、`stale` / `no-response` の件数が増えたら調べる

## 関連

- [[DR-0041]] 決定 3 / 4 / 6 — socket は `<状態の root>/sessions/<id>.sock`、同じ id の socket が残っていれば run は起動しない (生死は判定しない、片付けはこの runbook の経路)
- [[R5-H12]] — `panic = abort` を維持する判断
- `crates/hyoui/src/discovery.rs` の `query_status` — 接続拒否時の lock の確認と片付け
- `crates/hyoui/src/sys/socket.rs` の `impl Drop for UnixSock` — graceful exit 時の unlink
