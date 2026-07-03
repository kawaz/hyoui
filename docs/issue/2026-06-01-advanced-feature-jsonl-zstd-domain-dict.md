---
title: "Advanced feature idea: hyoui dump jsonl の自分ドメイン辞書付き zstd 圧縮 (`jsonl.zst`)"
status: idea
category: tech-memo
created: 2026-06-01T00:00:00+09:00
last_read: 2026-06-22T10:50:08+09:00
open_entered: 2026-06-01T00:00:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: kawaz 発言「自分ドメイン辞書付き zstd 内蔵で jsonl.zst とかを出力するみたいな発想もありかなとも思うがどうだろう」
---

# Advanced feature idea: hyoui dump jsonl の自分ドメイン辞書付き zstd 圧縮 (`jsonl.zst`)

- Priority: Low (= v0.3.0 以降検討、advanced future task)

idea 段階。DR-0016 (= `hyoui dump` MVP) で plain jsonl が安定したあと検討。

## 背景

`hyoui dump --format=jsonl` の出力は **terminal I/O bytes を hex string にして格納**するため、size 倍化 (+ jsonl 構造 overhead) で生 bytes の **2x〜3x** に膨らむ。

ただし terminal I/O は以下の理由で **圧縮効率が極めて高い**:

- ANSI escape sequence は同一 prefix (`\x1b[`) を高頻度で繰り返す
- 色指定 / cursor 操作 / clear 系の sequence は限られた語彙
- TUI app (= vim / claude / less) は同 frame 内で repeat byte 多発
- hex string にすると bytes 0x00-0xFF の出現が 16 文字 (`0`-`9` + `a`-`f`) に圧縮される = エントロピー低下

→ **hyoui 専用に pre-train した zstd 辞書**を使えば、`.jsonl` を `.jsonl.zst` にして **5x〜10x 圧縮**が期待できる (= 実測必要)。

## 設計案

### 1. `--format=jsonl.zst` を追加

```bash
hyoui dump start <session> --output session.jsonl.zst --format=jsonl.zst [--zstd-level=N]
```

- 出力は **gzip 互換の magic + zstd 圧縮ストリーム**
- zstd は **stream 圧縮** 対応なので append 書き込み可 (= rotate と統合可)
- daemon 側で `zstd::stream::Encoder` (= `zstd` crate) でラップ

### 2. ドメイン辞書の埋め込み

```
crates/hyoui/assets/zstd-dict-hyoui-tty.dict (= binary, ~16-64KB)
```

- terminal I/O コーパス (= 多種 TUI app の dump session を集めた sample) から **`zstd --train`** で生成
- daemon binary に `include_bytes!()` で埋め込み (= バイナリサイズ +50KB 程度)
- 復号時も同辞書必須 (= daemon と client が同一辞書を共有)
- 辞書 version 管理: 辞書 hash を header に含める (= 復号時に validate)

### 3. format header に圧縮情報

`hyoui-dump-jsonl/1` header の拡張:

```jsonl
{"v":1, "type":"hyoui-dump-jsonl-zstd/1", "session":"foo", ..., "compression":{"algo":"zstd","dict_sha256":"...","level":3}}
{"ts":..., "dir":"out", "bytes":"..."}
```

全部 zstd ストリーム化 + magic header で外側 tool が判別する設計が筋。

### 4. 復号 / 読み出し API

- `hyoui dump replay --input session.jsonl.zst` (= 復号後 plain jsonl を stdout に流す、外部 tool 連携用)
- `zstd -d` 単体でも復号可 (= 辞書 path を `--patch-from` で渡す経路、現実的には `hyoui dump replay` が推奨)
- 復号失敗 (= 辞書 mismatch) は明示的 error 出す

## 効果見積もり (= 要実測)

| シナリオ | 生 jsonl size | jsonl.zst (辞書なし) | jsonl.zst (辞書あり) |
|---|---|---|---|
| 静的 TUI app (= vim 開きっぱなし) | 100MB | ? | ? |
| 高 frame rate TUI (= top / btop) | 1GB | ? | ? |
| line-oriented session (= bash + 短 cmd) | 10MB | ? | ? |

実測は別 task。**期待値**: 辞書あり zstd で 5x-10x 圧縮、辞書なしでも 3x-5x 程度は出る。

## 実装上の検討点

### a. 辞書学習コーパスの確保

- hyoui daemon を回して各種 TUI / shell session の dump を集める
- corpus は public OSS で公開可能 (= terminal I/O bytes は基本情報少ない)
- `zstd --train` で 16KB / 64KB / 128KB の dict を生成、size vs 圧縮率の trade-off 測定

### b. 辞書の version 管理

- 辞書改訂 (= 新 TUI app 対応で再学習) があり得る
- header に `dict_sha256` を含め、復号時に dict mismatch を検出
- 旧版 dict は repo に残して `hyoui dump replay --legacy-dict=<sha>` で復号可能にする
- DR-0008 protocol design の cap flag 規約と整合させる (= dump 自体は file format、protocol cap とは独立)

### c. daemon 側 CPU / memory コスト

- zstd は速い (= zlib より 1 桁速い)、CPU は誤差
- memory: streaming encoder は内部 buffer 数 MB、辞書 dict 64KB、これは無視できる
- 落ちた場合の file 整合性: zstd stream は **frame 単位で flush** すれば壊れない、行末で frame end するなら 1 行損失で済む

### d. raw format も同様に圧縮するか

- `--format=raw.zst` も理論上可能だが、raw は単 direction (= stdin or stdout 一方) 用途で size 元々小さい
- → raw は plain で OK、zstd は jsonl 専用が筋

## 段階

| 段階 | 内容 |
|---|---|
| Phase 0 (= DR-0016) | `hyoui dump` MVP (= plain jsonl + raw)、本 issue は scope 外 |
| Phase 1 | コーパス収集 + 辞書学習 (= research、`docs/findings/` に圧縮率実測まとめ) |
| Phase 2 | `--format=jsonl.zst` 実装 (= dict 埋め込み + encoder/decoder) |
| Phase 3 | `hyoui dump replay` + 外部 tool 連携 |

## kawaz 確認ポイント

1. **辞書 size の許容範囲**: daemon binary に +50KB / +200KB 埋め込みは OK か (= 圧縮率と trade-off)
2. **dict version 管理**: header 埋め込み方式 (= dict_sha256) で十分か、より頑健な scheme 要るか
3. **コーパス公開**: 学習 corpus は公開 OK か (= terminal I/O bytes は機密性低いが、特定 TUI app の操作履歴含むなら要 sanitize)
4. **優先度**: Phase 1 (= 実測) を v0.3.0 までに着手するか、v0.4.0 以降に延期するか

## 関連

- [DR-0016](../decisions/DR-0016-tty-io-record.md) — `hyoui record` MVP (= jsonl 平文)
- [DR-0008](../decisions/DR-0008-protocol-design.md) — protocol cap flag 規約
- [DR-0013](../decisions/DR-0013-screen-emulator-and-attach-stability.md) — Phase C で zstd 言及 (= scrollback 圧縮、本 issue とは独立の zstd 用途)
- `zstd` crate (= Rust binding for libzstd)

## Triage (2026-07-03)

必要性の実例 (= dump jsonl のサイズが実運用で問題になった事例) が出るまで idea に落とす。
DR-0013 Phase C の zstd 検討と統合先を再評価するのはその時点で。
