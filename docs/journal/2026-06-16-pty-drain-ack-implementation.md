# 2026-06-16: PTY drain ack 機構の実装と統合

`hyoui input` の `text:<長文> → key:Enter` race の根本解決として PTY drain ack 機構 (DR-0021) を実装し、v0.7.0 として release した日の経緯。元 issue: `docs/issue/2026-06-16-bug-input-text-key-enter-not-sent.md` (= 本 journal で昇華済、issue file は delete)。

## 経緯ハイライト

1. **bug 報告**: cmux-msg → hyoui era 移行の動作練習で kawaz が観測。`hyoui input "wait-idle:500ms" "text:<長文>" "key:Enter"` で text は届くが Enter が落ちる
2. **仮説判定**: 3 仮説 (A: sequencing race / B: cap negotiation 競合 / C: bracketed paste 混在) を sub-agent で並列調査。**A 確定**、より正確には「bytes 系 spec の完了点が socket flush で、PTY drain を待っていない」という仕様の手抜き
3. **ワークアラウンド検証**: v0.6.5 では control 含め 9 回試行で全成功 = flaky。ただし実装の race 素地は確定 → 仕様レベルで根治することに合意
4. **方針合意**: kawaz と「PTY drain ack」採用。暗黙 wait-idle (= 「便利」枠、透過原則違反) ではなく spec 意味論の完成として ack 同期を入れる
5. **実装**: Opus sub-agent が `TYPE_RAW_ACK = 0x02` + daemon write_all 完了 ack + client ack 待ちを実装、cargo test 1017 pass
6. **regression 発見**: 実機マトリクス検証で python -i 1037 B+ で `frame decode failed while waiting raw_ack` が再現 (= 1024 B 境界)
7. **regression 原因 + 修正**: メインが `read_exact` の partial-byte discard race を実装読み解きで特定 (= `set_read_timeout` 下で `decode_from` が中途半端な read で TimedOut → partial bytes 破棄 → 次 iteration が size を誤解読)。**poll-based readiness + blocking decode** に書き換え
8. **並行 codex adversarial review**: major 2 (失敗 ack 切断で失われる / stale ack 誤受理) + minor 1 (recv_control の unsolicited RAW_ACK) を検出 → 修正 agent に統合依頼 → 全部 fix
9. **MANUAL 更新**: ICANON 1024 B 制約 + ack error code 一覧を §4 に追記
10. **派生 issue 2 件起票**: ICANON apps の chunk 化 helper / ack test cover 拡張 (= 本 PR scope 外、wip)
11. **統合 + push**: `jj split` で fix-related 16 file のみを fix commit に固定、無関係 issue 2 file は別 branch (quossnmz) に隔離。v0.6.5 → v0.7.0 minor bump (protocol breaking、v1.0 前許容)
12. **push の罠を 3 連続で踏む**: `cargo fmt` 未通過 → `crates/hyoui-cli/Cargo.toml` の `hyoui = "^0.6"` 残置 → `Cargo.lock` 漏れ。それぞれ `jj edit / squash --into` で fix commit に統合
13. **v0.7.0 release**: CI / Release workflow 両 success、tag + GH Release が release.yml で自動作成

## ハマり所 → 解決策ペア

### partial-byte race (sub-agent 実装の見落とし)

- **現象**: python -i 1038 B+ で `frame decode failed while waiting raw_ack`、vim 2000 B では成功
- **誤った原因仮説** (検証 agent): python の bracketed paste 応答が daemon → client の RAW_ACK frame と衝突
- **真の原因** (メイン読み解き): `read_exact` の `TimedOut` 時 partial-byte discard 仕様。`set_read_timeout(RAW_ACK_TIMEOUT)` 下で `decode_from` が body 読み中に timeout → 部分読みの bytes 破棄 → continue 後の次 iteration で body の途中 4 bytes を size として誤解読 → `FrameTooLarge` or `UnknownType` → Protocol error
- **修正**: `recv_raw_ack_inner` を `poll(2)` readiness 待ち → 読める時に blocking `decode_from` で完走、に書き換え。socket の `read_timeout` を一切変更しない。partial-byte discard を構造的に絶滅

### stale ack 誤受理 (codex 指摘)

- **現象**: `send_raw_bytes` が timeout 後も同じ接続を再利用すると、遅れて届いた前回 ack を次回 ack として誤受理。seq id 無しなので区別不能
- **修正**: `ClientConnection.poisoned: bool` + `poison()` helper を追加。timeout/I/O/protocol error で接続を poison → 次回 `send_raw_bytes` で `Error::Invalid("connection poisoned...")` を返して再利用拒否。`Err(Remote(_))` (= protocol-valid な ack:Error) は poison しない (= legitimate な業務エラー、再利用可)

### 失敗 ack が DropClient で失われる (codex 指摘)

- **現象**: daemon が partial/timeout で `send_raw_ack(Error)` を enqueue した直後に `ClientFrameOutcome::DropClient` を返す → `ClientHandle::Drop` が shutdown + writer_tx close → ack frame が flush 前に socket が閉じる
- **修正**: `ClientHandle::Drop` の order を反転: **writer_tx drop (= writer_pump が queued frames drain) → `set_write_timeout(DROP_FLUSH_TIMEOUT=500ms)` → join → `shutdown(Both)`**。失敗 ack が flush されてから shutdown

### push の罠 3 連続

| 罠 | 原因 | 修正 |
|---|---|---|
| `cargo fmt --check` fail | sub-agent コードが fmt 未通過 | `jj edit skpkolsx` → `cargo fmt` → `jj edit @++` で戻る |
| `hyoui-cli` の `hyoui = "^0.6"` 不一致 | workspace.package.version は 0.7.0 にしたが crate 間 dep は手動更新が必要 | `Edit crates/hyoui-cli/Cargo.toml "^0.6" → "^0.7"` → `jj squash --into skpkolsx` |
| `Cargo.lock` 未 commit | `cargo test --release` 時に lock 更新が走った | `jj squash --into skpkolsx Cargo.lock` |

→ 学び: **agent に実装させた後、push 前にメインで `cargo fmt && cargo build --release` を 1 回回しておく**と push 経路の rework を減らせる。今後の standard procedure 候補。

### `ensure-clean` の罠

- `just push` の deps `ensure-clean` は **@ が empty change** を要求 (`bump-semver vcs is clean`)
- `cargo test --release` を直接走らせると `Cargo.lock` が変わり @ が dirty 化
- → ensure-clean fail。Cargo.lock を fix commit に squash すれば回避

### jj split の description 配分

- `jj split -m "MSG" <files>` は **selected (= file 指定)** に MSG description を与え、**remaining** は original description を保持
- 本 PR では split 前の `jj describe @ -m ""` で元 description を空にしてから split することで、selected = DR-0021 desc / remaining = no-desc にした

## 設定値・コマンド snippet

### protocol 完了点を変えた core 部分

```rust
// crates/hyoui/src/client/attach.rs (send_raw_bytes)
pub fn send_raw_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
    if self.poisoned { return Err(Error::Invalid("connection poisoned ...")) }
    if bytes.is_empty() { return Ok(()) }
    // socket write (no read_timeout 変更)
    Frame::raw_data(bytes.to_vec()).encode_to(&mut self.writer)?;
    self.writer.flush()?;
    // poll-based ack 待ち
    let r = self.recv_raw_ack_inner();
    if matches!(&r, Err(Error::Io(_)) | Err(Error::Invalid(_))) { self.poison() }
    r
}
```

### 実機マトリクス検証コマンド

```bash
cargo build --release
SESS=$($PWD/target/release/hyoui run --detached --namespace=ack-re-test -- python3 -i)
sleep 0.5
LONG=$(printf 'A%.0s' {1..900})
$PWD/target/release/hyoui input "$SESS" --namespace=ack-re-test \
  "text:print('$LONG END')" "key:Enter"
sleep 1
$PWD/target/release/hyoui tail "$SESS" --namespace=ack-re-test --last-bytes=3000 | tail -10
$PWD/target/release/hyoui stop "$SESS" --namespace=ack-re-test
```

### push 経路

```bash
jj describe @ -m ""   # split 前に元 desc を空に
jj split -m "DR-0021: ..." <fix files...>
jj edit sunosqww      # 空 @ に戻る
bump-semver minor Cargo.toml --write   # version 0.6.5 → 0.7.0
jj describe -m "chore(release): bump version to 0.7.0 ..."
jj new -m "wip: push 前 empty change"
just push             # deps: ensure-clean / ci / translations / version-bumped
```

## 議論の要点

- **暗黙 wait-idle を採用しなかった理由**: 「子が一定時間出力しなかった」は drain 完了の十分条件ではない (= cap negotiation 中の echo 待ち等で false negative)。透過原則 (DR-0014) からも「便利寄り」介入は不採用
- **seq id を ack body に持たなかった理由**: 「1 invocation の input spec を順次送る」semantics で十分。pipeline は forward-compat に `raw-pipeline-v1` cap として後で導入可能。`pending_frames` の延長で実装容易
- **cap flag (= `raw-ack-v1`) を導入しなかった理由**: v1.0 前 (= breaking 許容)、単一バイナリ配布で daemon と client の version skew は基本起きない。cap の維持コストが利得を上回る

## ICANON apps への大量 byte 送信が表に出た副次効果

DR-0021 で完了点が明示 ack に変わったことで、これまで socket flush の race で覆い隠されていた **line discipline 1024 B + `MASTER_WRITE_IDLE_TIMEOUT_MS = 500ms`** 制約が表に出た。

- 旧版: silent drop (= exit 0 だが text 欠損 + Enter 未到達)
- 新版: `master.write-timeout` で明示失敗 (= ack:Error)

これは bug の覆い隠しが解けた **正常な挙動**で、本物の制約。chunk 化 helper / timeout 調整は別 issue (`docs/issue/2026-06-16-feature-icanon-large-input-chunking.md`) で対応。

## 関連

- DR-0021: PTY drain ack for bytes input
- DR-0006 §8.6: spec sequencing 完了点 (本 DR-0021 で明文化)
- DR-0008: protocol、`TYPE_RAW_ACK = 0x02` 追加
- DR-0014: 透過原則 + 検証主義 (self-check 全項目通過)
- DR-0016: `MASTER_WRITE_IDLE_TIMEOUT_MS = 500ms` (周辺仕様)
- `docs/issue/2026-06-16-feature-icanon-large-input-chunking.md`: 副次効果の継続課題
- `docs/issue/2026-06-16-feature-ack-test-coverage-expansion.md`: codex review minor 由来
- v0.7.0 release (= main `9c4ef2dc` chore bump + `6635b5c7` DR-0021 fix)
