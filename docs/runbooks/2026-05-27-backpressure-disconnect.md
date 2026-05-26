# backpressure による client 切断時の確認手順

> Status: Active
> Date: 2026-05-27
> Related: [[DR-0008]] §8.2 (client buffer)、[[R5-SRE-C1]] (観測手段)

## 症状

- client (= attach 中のプロセス) が `hyoui` daemon から **断続的に切断**
  される、または出力の取りこぼしが起きる
- 高負荷の child (= `yes` や巨大ログ吐き) を抱えた session に多数の
  client が attach している
- `hyoui status <session>` 等の control message で `queued_bytes` が
  `client_buffer_bytes` の上限近くに張り付いている
- 切断は単一 client のみで、他の client は生きている (= per-client buffer
  上限の発火)

## 切り分け

1. status 経由で各 client の `queued_bytes` を観測:
   ```bash
   hyoui status <session>           # client 一覧、queued_bytes 列
   # client 単位の queued_bytes が buffer_limit (= client_buffer_bytes、default 8 MiB) 近傍なら backpressure 発火中
   ```
2. 該当 client が slow consumer かを確認:
   - `strace -p <client_pid> -e read` で socket read が止まっているか
   - client 側の処理 (= 親 process の標準入出力先) で書き込みブロック
     しているか (= 例: パイプ先が満杯)
3. daemon の起動オプションを確認:
   ```bash
   ps -o args= -p <daemon_pid>      # --client-buffer-bytes が指定されているか
   ```
   省略時は `DaemonConfig::default()` の 8 MiB が使われている (= 多数 client
   や巨大 frame で不足し得る)。
4. broadcast 経路の amplification を疑う (= R5-H9 backlog):
   - N client へ同一 frame を `Vec::clone()` で配ると O(N × frame_size)。
     frame_size が大きい (= 1 MiB) かつ N=10 を超えると 10 MiB/frame の
     memory bandwidth を消費し、slow client の queue を即座に押し出す

## 対処

1. **即時**: 切断された client を再 attach (= daemon 側は client 切断後
   も session を維持する)
2. **設定変更**: daemon 起動時 `--client-buffer-bytes <N>` で per-client
   buffer を増やす:
   ```bash
   # 例: 32 MiB に拡大 (= 高負荷 child + slow client 想定)
   hyoui run --client-buffer-bytes 33554432 <session> <command>
   ```
   - 増やせばいいというものではない。N client × buffer_bytes が daemon
     の RSS 上限になる。`client_buffer_bytes × MAX_CLIENTS_PER_DAEMON`
     が物理メモリの妥当な割合に収まる値を選ぶ
3. **根本対処**: slow consumer 側の処理を高速化、または非同期化:
   - パイプ先のドレインを別 thread で行う
   - 出力を間引く (= 巨大ログ吐きの child は本来 hyoui の用途外)
4. daemon ログ (R5-SRE-C1 対応後) で disconnect の `reason=backpressure`
   を grep:
   ```bash
   grep 'reason=backpressure' "$XDG_STATE_HOME/hyoui/<session>.log"
   ```

## 予防

- attach する client は **常に socket を drain する**設計にする (= 自前で
  read を止めない、止める場合は backpressure で切られる覚悟をする)
- 巨大出力を吐く child は hyoui で wrap しない (= multiplex で broadcast
  が amplify する。直接ファイルにリダイレクトすべき)
- buffer 上限はワークロードに応じて事前に measurement (= R4-M28 backlog)
- 監視: `queued_bytes / buffer_limit` 比率を継続観測、80% 超過で
  警告 (= disconnect の predictor)

## 関連

- [[DR-0008]] §8.2 — client_buffer_bytes の既定値根拠と framing
- [[R5-H9]] — broadcast amplification (Vec::clone × N) 解消の backlog
- [[R5-SRE-C1]] — 構造化ログ整備 (disconnect reason を残す)
- `crates/hyoui/src/daemon/broadcast.rs` — `try_enqueue_frame`、
  `queued_bytes` の atomic 加減算
- `crates/hyoui/src/daemon/config.rs:58` — `client_buffer_bytes: 8 MiB`
  default
