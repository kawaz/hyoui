# Feature idea: claude code TUI 自動操作 (A/B/C 判定 + L1/L2 必須要件)

- Status: Open (Idea, L1/L2 実装待ち)
- Date: 2026-05-26
- Priority: High (hyoui の本命ユースケース、ただし実現は v0.2.0/v0.3.0)
- 発見元: 2026-05-26 CLI 設計議論 (Phase 11 wait L0 詳細) で「claude のような TUI app の自動操作が本格 use case」と判明

## 背景

hyoui の主用途として claude code (またはその他 TUI app) の自動操作が想定される。
MVP の wait L0 ([[DR-0006]] Section 11) では半分動くが、claude の状態判定を確実に行うには
L1 (画面 emulator) と L2 (named area + 述語) が必要。本 issue で具体要件を集約。

## 判定したい状態

claude code TUI の入力欄状態:

```
────────────────────────────────────────────
↑ ここから 2-3 行上に "✻ Brewed for 1m 46s" や "✳ Boogieing… (29s · thinking more with high effort)" 等が出る
────────────────────────────────────────────
❯ <cursor>push しとく   ← サジェスト (graytext)
────────────────────────────────────────────
```

- **状態 A: 考え中 + 入力待ち**
  - 入力欄は空 (カーソル位置 = `❯ ` の直後)
  - 2-3 行上に進行表示 (`✶/✳/✷/✦` 等のアニメ文字が連続更新、tty 書き込みが継続発生)
  - = claude が裏で処理中

- **状態 B: ユーザ入力待ち**
  - 入力欄は空 (カーソル位置 = `❯ ` の直後)
  - 進行表示なし、tty 書き込みは完全停止 = idle
  - = claude が完全に手待ち、ユーザの次の入力を待ってる

- **状態 C: 入力中**
  - 入力欄に文字あり (カーソル位置が `❯ ` から数文字後にずれてる)
  - = ユーザが何か打ち途中

自動操作の use case: **「A or B」(= 入力欄が空) を狙って `hyoui keys` で入力を差し込みたい**。

## 各状態の判定可能性

| 状態 | L0 で判定可? | 必要な機能 |
|---|---|---|
| **B** (完全 idle) | ✅ `--idle 5s` で確実 | tty 書き込み 5 秒停止 |
| **A** (考え中) | ⚠️ 脆弱、pattern match のみ | 「直近 N 秒に `✶/✳/✷/✦` 等が複数回出現」= 時系列 pattern (MVP 不足) |
| **A or B** (入力欄空) | △ `--idle 1s` で近似 | C のタイピング合間と誤認リスク |
| **C 除外** | ❌ 不可 | カーソル位置必須 = L1 emulator |

## L0 (MVP) で書ける範囲

```bash
# B 判定 (確実、5 秒 idle で「ほぼ B」)
hyoui wait <name> --idle 5s

# A or B 近似 (誤認リスクあり)
hyoui wait <name> --idle 1s

# 思考完了 marker (claude 固有、L0 で確実)
hyoui wait <name> --pattern '^✻ \w+ for' --then-idle 1s
```

claude の **「思考完了」検知は L0 で実用可能** (`✻ Brewed for ...` / `✻ Sautéed for ...` 等の completion marker)。
ただし `Brewed`/`Sautéed`/`Simmered` の動詞部分はランダム調理動詞、`✻` プレフィックスは固定っぽい。

`hyoui tail --since DUR | grep` で A 判定の補助も可:

```bash
# A 判定 (直近 2 秒で アニメ文字 出てるか) - tail との組み合わせ
hyoui tail <name> --since 2s --strip | grep -qE '[✶✳✷✦]|Boogieing|thinking' && echo A
```

## L1 (v0.2.0) 必要な要件

claude 自動化を本格化するために必須:

1. **alternate screen 対応**: claude code は alternate screen 使用 (= フルスクリーン TUI)。daemon は primary/alternate を別 grid で管理:
   ```bash
   hyoui tail <name> --screen=primary       # primary 出力履歴
   hyoui tail <name> --screen=alternate     # alternate 現在の状態 (snapshot 相当)
   hyoui wait <name> --screen=alternate --pattern '...'
   ```
2. **画面 rect 指定 wait**: 入力欄行 (= 特定 row) の内容を検査:
   ```bash
   hyoui wait <name> --rect 0,LAST_ROW,COLS,1 --text "❯ " --then-idle 5s
   ```
3. **cursor 位置検査**: C 除外 (= `❯ ` の直後にカーソルがあるかで A/B と C 区別):
   ```bash
   hyoui wait <name> --cursor-after "❯ " --then-idle 5s
   ```
4. **snapshot 機能**: 画面状態を 1 回 dump して shell 側で検査:
   ```bash
   hyoui snapshot <name> --rect 0,-3,COLS,3 --format text
   ```

## L2 (v0.3.0) で書きたいレベル

```bash
# named area + 複合述語
hyoui wait <name> --area input-line --predicate-file ./claude-prompt-idle.json --then-idle 5s
```

`./claude-prompt-idle.json`:
```json
{
  "all": [
    {"area": "input-line"},
    {"cursor": {"col_offset_from_start": 2}},
    {"line_chars": {"from_col": 2, "attr": {"fg": "gray"}}}
  ]
}
```

これで「入力欄行 + カーソル `❯ ` 直後 + それ以降は graytext のみ (= サジェスト出てる or 空)」を 1 wait で判定。

## 段階優先度

- **v0.1.0 MVP**: L0 で `--idle 5s` (B) + Brewed/Sautéed completion (一部 A) + tail で A 補助。**実用 5 割程度**
- **v0.2.0 (L1)**: alternate screen + cursor 位置 + rect 指定。**実用 8〜9 割**
- **v0.3.0 (L2)**: named area + JSON 述語。**実用 100%、設定で柔軟**

## 関連

- [[DR-0005]] — 外側自動操作主軸の思想 (本要件は思想ど真ん中)
- [[DR-0006]] — wait L0 仕様、L1/L2 拡張可能性
- [[DR-0007]] — v0.2.0 で L1 emulator、v0.3.0 で L2 述語の段階
- `docs/issue/2026-05-26-feature-recording-and-dump.md` — record/play で自動化シーケンスを記録
- `docs/journal/2026-05-26-cli-design-discussion.md` — Phase 11 で本要件の発見経緯
