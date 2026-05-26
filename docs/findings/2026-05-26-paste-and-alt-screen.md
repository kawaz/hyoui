# Finding: bracketed paste と alternate screen の自動有効化 + escape 観測

- Date: 2026-05-26
- PoC: `crates/hyoui/examples/03-paste-and-alt-screen.rs`
- 関連: [[DR-0006]] §10 (paste), §11 (wait), §11.6 (scrollback)

## 判明した事実

### bracketed paste の自動検出が筋

- **bash -i (== interactive shell 一般) は起動時に `ESC[?2004h` を自動出力** (= bracketed paste mode を有効化要求)
- 同様に zsh / fish も readline 系 line editor が初期化されたタイミングで出すはず (= shell が rc 読み終わって prompt 出す前)
- daemon が子の出力 stream から `ESC[?2004h` / `ESC[?2004l` を track して内部 bracketed_mode フラグを更新する [[DR-0006]] §10 の方針は妥当 = 実装可能

### TUI app は alternate screen を使う

- **vi 起動で `ESC[?1049h` (alternate screen enable) + `ESC[?2004h` (bracketed paste) 両方出力**
- alternate screen 中の出力 rate = 起動時 1810 B/s (= 2733 bytes / 1.5s)、起動 burst のみで定常はもっと少ない
- claude code / ratatui ベースの TUI も同様 (crossterm の `EnterAlternateScreen` がこれ) と推定
- alternate screen 中の escape 種別: cursor 移動 (`ESC[<row>;<col>H`)、SGR (`ESC[<n>m` 系の color/attr)、cursor visibility (`ESC[?25h`/`l`)、erase (`ESC[K`)、特殊 (`ESC[7m` reverse, etc.)

### less は alternate screen を使わない (この実行では)

- `less /etc/passwd` 起動で `ESC[?1049h` 出力なし
- `ESC[?1h` (DECCKM = カーソルキー application モード) + `ESC=` (keypad application モード) は出力
- less は `LESS=-R` 等の env で挙動変わる、また version によっても違う
- 「TUI app だから必ず alternate」ではない (= 個別判定要)

### 実装の罠

1. **`Pty::spawn` で sh -c 経由は混乱の元**: `sh -c "zsh"` で起動すると zsh の rc が重くて 2 秒では prompt まで届かない (kawaz の zsh は prezto/oh-my-zsh 等? 詳細未確認)。直接 `["zsh"]` を exec、または `["bash", "-i"]` のように argv 分割で起動する方が確実
2. **bash の引数解析の癖**: `bash -i --norc --noprofile` を試したが「`bash: --: 無効なオプション`」エラー (理由未追跡、たぶん bash version / argv 順序の問題)。シンプルに `bash -i` で動く
3. **nonblocking read の loop**: read が EAGAIN を返すたびに sleep 10ms、データが来た時点で読む、EOF (= 子 exit) で break。poll を使えばより効率的だが、PoC では sleep ループで十分

## 実用的な示唆

### bracketed paste auto-detect の実装方針

```rust
// daemon の output stream parser (= 子 → master の bytes を分析)
struct OutputState {
    bracketed_paste_active: bool,
}
impl OutputState {
    fn observe(&mut self, bytes: &[u8]) {
        for w in bytes.windows(8) {
            if w == b"\x1b[?2004h" { self.bracketed_paste_active = true; }
            if w == b"\x1b[?2004l" { self.bracketed_paste_active = false; }
        }
    }
}
```

`paste` コマンドで bracketed paste 適用するかは `bracketed_paste_active` フラグで判断。`--bracketed-paste=auto` (default) ならフラグ参照、`on`/`off` ならフラグ無視。

### scrollback size の根拠材料

- 通常 shell (bash -i): 256 B/s (起動時)、定常は 0 (= prompt 待ち)
- TUI 起動 (vi): 1810 B/s (起動 burst)、定常は数十〜数百 B/s (= 編集中)
- 高頻度更新 TUI (claude のアニメ思考中): 未計測、CLI 設計議論で推定 30 KB/分 = 500 B/s 程度

= **default 4MB scrollback で claude 用途 2 時間以上**、shell 用途は半永久。妥当。

### alternate screen 切替時の bytes 量

vi 起動: 2733 bytes/1.5s = 1.8 KB/s だが、これは「初期描画」(画面全体の 1 回 paint) + 「prompt 待ち idle」。
**初期 burst (alternate enter + 全画面描画) で数 KB 一気に流れる**、定常 idle は 0、編集操作で数百 B/s。

ring buffer が小さい (= 64KB 等) と initial paint だけで埋まる可能性。default 4MB なら余裕。

### claude code (未計測、推定)

ratatui / crossterm ベースの TUI は:
- 起動時に `EnterAlternateScreen` (`ESC[?1049h`)
- 描画は frame ごとに「変更セルだけ」更新 (= delta rendering)、ただし初回は全画面
- アニメ ("✶ Brewed..." 等) は 100-200ms 周期で「アニメ部分の cell」だけ更新
- 1 アニメ更新 ~50 bytes (cursor 移動 + 1 文字 + SGR)
- 10Hz × 50 bytes = 500 B/s 程度 (claude 思考中)
- 大量出力 (= response stream) は別、数 KB/s burst

実観測は **手動で `cargo run --example 03-paste-and-alt-screen -- claude 10000`** で可能 (PoC が claude 起動 → 10 秒観測 → kill)。kawaz の claude 環境で起動できれば。

## hyoui 本実装への反映

### daemon の output state machine

```rust
struct PtyState {
    bracketed_paste_active: bool,
    alternate_screen_active: bool,
    // 将来: cursor位置, screen grid (L1)
}
```

`master.read()` で得た bytes を全 client に broadcast する**前に** state machine に通す:

```rust
fn handle_master_read(&mut self, bytes: &[u8]) {
    self.pty_state.observe(bytes);          // state 更新
    self.broadcast_to_clients(bytes);        // そのまま中継 (escape も流す)
    self.scrollback.push(now(), bytes);     // ring buffer 蓄積
}
```

bracketed_paste_active は paste コマンドで使う。alternate_screen_active は MVP では使わないが、status コマンドの表示・将来の L1 (画面 emulator) で必要。

### escape parser の単純化

PoC の `extract_escapes` は CSI/OSC/DCS/single の簡易 parser、不完全 (= partial sequence の handling 未対応)。本実装の output state machine は **完全な vt100 parser** が理想だが、最初は「特定 escape (?2004h/l, ?1049h/l) だけを byte slice match」で十分 (= L0 範囲、L1 emulator は別 crate `vte` 等を使う)。

### 装飾除去 (PoC 08 で詳細) との関係

wait/match の装飾除去 default = ANSI escape strip ([[DR-0006]] §11)。strip 対象は CSI/OSC/DCS/single char すべて、PoC 03 で観測した 13-127 種の escape を全部 strip 対象に。PoC 08 で regex を確定。

## 検証の詳細

### Test 1: bash -i

```
$ cargo run --example 03-paste-and-alt-screen -- "bash -i" 2000
=== observe 'bash -i' for 2000ms ===
  read counts: data=3, EOF=0, EAGAIN/errs=161
=== observed 516 bytes in 2.011s ===
  byte rate: 256.5 B/s
=== detected ===
  bracketed paste enable  (ESC[?2004h): true
  alt screen enable       (ESC[?1049h): false
=== escapes (25 total, 13 unique) ===
  10 × ESC[0m, 3 × ESC[32m, 2 × ESC[1;33m, 1 × ESC[?2004h, etc.
```

### Test 2: vi

```
$ cargo run --example 03-paste-and-alt-screen -- "vi /tmp/poc-vi-test" 1500
=== observe 'vi /tmp/poc-vi-test' for 1500ms ===
  read counts: data=5, EOF=0, EAGAIN/errs=119
=== observed 2733 bytes in 1.509s ===
  byte rate: 1810.3 B/s
=== detected ===
  bracketed paste enable  (ESC[?2004h): true
  alt screen enable       (ESC[?1049h): true
=== escapes (127 total, 84 unique) ===
  21 × ESC[7m, 20 × ESC[23;63H, ESC[?25h, ESC[?25l, cursor 移動多数...
```

### Test 3: less

```
$ cargo run --example 03-paste-and-alt-screen -- "less /etc/passwd" 2000
=== observe 'less /etc/passwd' for 2000ms ===
  read counts: data=2, EOF=0, EAGAIN/errs=161
=== observed 1234 bytes in 2.000s ===
  byte rate: 616.7 B/s
=== detected ===
  bracketed paste enable  (ESC[?2004h): false
  alt screen enable       (ESC[?1049h): false
  ESC[?1h (DECCKM): true
  ESC= (keypad app): true
=== escapes (28 total, 6 unique) ===
  23 × ESC[m, 1 × ESC[7m, 1 × ESC=, 1 × ESC[?1h, 1 × ESC[27m, 1 × ESC[K
```

less は alternate を使わず、application keypad mode で画面を埋める方式。env (`LESS`) でも挙動変わる。

### claude code の観測 (未実施)

`cargo run --example 03-paste-and-alt-screen -- claude 10000` で観測予定 (kawaz の claude 環境で起動できる場合)。

期待:
- `ESC[?1049h` (alternate)
- `ESC[?2004h` (bracketed paste)
- アニメ中の出力 rate (推定 500 B/s)

これは別途検証 (= 本 PoC スコープ外、推定で十分)。
