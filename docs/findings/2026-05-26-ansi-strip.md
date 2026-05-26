# Finding: ANSI escape strip (CSI / OSC / DCS / single char)

- Date: 2026-05-26
- PoC: `crates/hyoui/examples/08-ansi-strip.rs`
- 関連: [[DR-0006]] §11 (wait L0 装飾除去)

## 判明した事実

1. **手書き state machine (50 行) で CSI / OSC / DCS / single char escape を全 strip 可能** (依存ゼロ、regex 不要)
2. **PoC: synthetic test 11 ケース全 PASS + 実 sample で ESC 残留 0**
3. **実 sample の retention rate**:
   - bash prompt: 516 → 383 (74.2% 保持、装飾 25.8%)
   - vi 起動初期: 2733 → 1819 (66.6% 保持、装飾 33.4%)
   - less: 1234 → 1146 (92.9% 保持、装飾 7.1% = `ESC[m` 系の SGR が主)
4. **strip 後の text は visible chars 94-100%** = ほぼ全文字が text として有効、wait/match で使える品質

## 実用的な示唆

### state machine の仕様 (簡易ですが十分)

```
ESC を見たら次の 1 byte で判定:
  '['  → CSI: param bytes (0x30..=0x3f) → intermediate (0x20..=0x2f) → final (0x40..=0x7e)
  ']'  → OSC: terminate by BEL (0x07) or ST (ESC \)
  'P' | 'X' | '^' | '_' → DCS/SOS/PM/APC: terminate by ST
  その他 → single char escape (ESC + 1 char、例: ESC=, ESC>, ESCM)
```

C0 (0x00-0x1f, except 0x09/0x0a/0x0d) や C1 (0x80-0x9f) の制御文字も strip するかは要判断:
- BEL (0x07): 残す? strip?
- BS (0x08): 残す? strip? (vi の cursor 戻しが BS で来ることもある)
- TAB (0x09): 残す (visible whitespace)
- LF (0x0a), CR (0x0d): 残す (改行)
- 他: strip 候補

PoC 実装では **ESC 系のみ strip、C0/C1 はそのまま**。これで bash/vi/less の sample は ESC 残留 0、text 94-100%。実用上問題ない。
ユーザ要求次第で `--aggressive` 等で更に strip 可能 (= 後付け option)。

### CRLF → LF 正規化 (装飾除去とは別)

[[DR-0006]] §11 で「装飾除去の一部として CRLF → LF も含める」と書いたが、PoC 結果を見ると **改行コードはそのまま残してる方が筋**:
- bash/vi の出力には `\r\n` が混在 (pty の ONLCR 効果、または app 自身の出力)
- wait/match の text/pattern では `\r?\n` で書く規約にすれば差が吸収できる
- 装飾除去 = ANSI escape のみ、改行は別判断、と分けたほうが責務クリア

[[DR-0006]] の Section 11 を update して「CRLF → LF 正規化は装飾除去に含めない、別 flag (将来検討)」に修正する候補。

### L0 の限界 (再確認)

PoC で観察: vi の出力には cursor 移動 escape が多数 (`ESC[2;34H` 等)。strip で escape は消えるが、**cursor 移動を反映した実画面の配置は再現できない**。

例: vi の "~" (empty line marker) は画面上で縦に並んでるが、stream 順では順次出力 + cursor 移動の混在。strip しても "~" が縦に並んだ視覚を取り戻せない。

→ L0 strip は「stream に出てきた text の連続」を返す、**実画面の grid 復元には L1 emulator が必要**。これは [[DR-0006]] §11 で既に明示済の制限。

実用的には:
- wait `--pattern "Brewed for"` のような **substring match は L0 で十分** (= text として stream に出れば拾える)
- wait `--cursor-at` / `--rect` (L1) は v0.2.0 で

## hyoui 本実装への反映

### `crates/hyoui/src/strip.rs` (新規 module、PoC 実装そのまま)

```rust
pub fn strip_ansi(input: &[u8]) -> Vec<u8> { /* PoC 実装 */ }

pub fn strip_ansi_into(input: &[u8], out: &mut Vec<u8>) { /* in-place 版 */ }

/// イテレータ版 (= stream で来る bytes をリアルタイム strip、partial sequence 対応)
pub struct StripAnsiStream { state: State, partial: Vec<u8> }
impl StripAnsiStream {
    pub fn feed(&mut self, input: &[u8], out: &mut Vec<u8>);
}
```

最初は `strip_ansi(&[u8]) -> Vec<u8>` の 1 関数で MVP、stream 版は wait/tail で「リアルタイム strip しながら pattern match」したい時に追加 (= partial sequence handling が必要、複雑度中)。

### wait/match での適用

```rust
fn match_text(stream_bytes: &[u8], pattern: &str, raw: bool) -> Option<usize> {
    let haystack = if raw {
        stream_bytes.to_vec()
    } else {
        strip_ansi(stream_bytes)   // 装飾除去 default
    };
    // ... substring or regex match on haystack
}
```

## 検証の詳細

### Synthetic tests (11 ケース)

```
  PASS  no escape
  PASS  SGR color                          (\x1b[31mRed\x1b[0m)
  PASS  clear screen + cursor home         (\x1b[2J\x1b[H)
  PASS  OSC title (BEL terminated)         (\x1b]0;title\x07)
  PASS  OSC title (ST terminated)          (\x1b]0;title\x1b\\)
  PASS  DCS                                 (\x1bP1$rdcs\x1b\\)
  PASS  single char (keypad app mode)      (\x1b=)
  PASS  bracketed paste enable/disable     (\x1b[?2004h ... \x1b[?2004l)
  PASS  alternate screen enable/disable    (\x1b[?1049h ... \x1b[?1049l)
  PASS  cursor positioning                 (\x1b[12;34H)
  PASS  multi-param SGR                    (\x1b[1;31;47m)
```

### 実 sample (PoC 03 で取得)

```
=== bash prompt (interactive shell) ===
  raw 516 → stripped 383 (74.2% retained, 25.8% 装飾)
  ESC remaining: 0
  visible/text: 94.8%
  preview: "bash: bind: ... command not found: gfind↩⏎...kawaz-mbp16-20211217:kawaz/hyoui/main via ..."

=== vi (alternate screen TUI) ===
  raw 2733 → stripped 1819 (66.6% retained, 33.4% 装飾)
  ESC remaining: 0
  visible/text: 98.7%
  preview: vi の "[New]" や "~" (empty line markers) が並ぶ

=== less (application keypad mode) ===
  raw 1234 → stripped 1146 (92.9% retained, 7.1% 装飾)
  ESC remaining: 0
  visible/text: 100.0%
  preview: /etc/passwd の内容 (装飾は ESC[m 系のみ)
```

全 sample で **ESC 残留 0** = strip 完全。装飾比率は app の種類に応じて 7-33% (= TUI ほど装飾多い)。
