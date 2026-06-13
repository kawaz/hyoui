# feature: ICANON apps への大量 byte 送信時の chunk 化 helper / timeout 調整

- Date: 2026-06-16
- Status: open
- Severity: 低 (= 既存挙動を悪化させていない、ack 機構で明示エラー化したことで露出した既知制約)
- Reporter: kawaz / Claude (DR-0021 PTY drain ack 実装中に露出)
- Related: DR-0021, DR-0016 (MASTER_WRITE_IDLE_TIMEOUT_MS), `docs/issue/2026-06-16-bug-input-text-key-enter-not-sent.md` (= DR-0021 起因 issue、本 issue 起票元)

## 背景

DR-0021 で bytes 系 input spec の完了点が「daemon の PTY drain ack 受信」に変わったことで、これまで socket flush の race で覆い隠されていた **line discipline 制約** が表に出た。

実機検証 (DR-0021 統合時):

| シナリオ | 結果 |
|---|---|
| python -i + 500 B + Enter | OK |
| python -i + 900 B + Enter | OK |
| python -i + 1037 B + Enter | NG (`master.write-timeout`) |
| python -i + 1038 B + Enter | NG (`master.write-timeout`) |
| python -i + 2000 B + Enter | NG (`master.write-timeout`) |
| vim 2000 B (alt screen, ICANON 無し) | OK |

## 根本制約

ICANON モード (= bash / python / sh などが default で使う行編集モード) は kernel 内に約 1024 B の line discipline buffer を持ち、`\n` か EOF が来るまで子に bytes を渡さない。1 行が 1024 B を超えると buffer がオーバーフローし、daemon 側の `write_all_with_idle_timeout` が **`MASTER_WRITE_IDLE_TIMEOUT_MS = 500 ms`** 内に forward progress を取れず timeout する。

これは line discipline の物理制約であり、ack 機構の bug ではない。DR-0021 以前は race で覆い隠されていただけ (= 旧バイナリでは exit 0 が返るが text 後半 + Enter が silent drop されていた)。

## 影響

- AI agent / cmux-msg / 自動化 script が ICANON シェル (bash / python REPL / sh) に 1 KB 超の prompt を 1 spec で送ると `master.write-timeout` で fail
- vim / claude / less などの alt screen TUI は ICANON 無効なので影響なし
- 旧バイナリで silent に動いていた人は新バイナリで明示 error を見る = 行動変化が必要

## 対応候補

### 案 A: spec 内で auto-chunk

`text:` spec の bytes が閾値 (= 例 512 B) を超えたら、内部で `\n` 区切りに分割して chunk ごとに ack 同期する。ユーザは何も書き換え不要。

懸念:
- 透過原則 (DR-0014) 違反の可能性。spec の wire 表現と動作が乖離
- `\n` 区切りで分割するのは text の意味論に依存 (= 改行を含まない巨大 text はどうするか)

### 案 B: 専用 spec `chunk-text:` を新設

明示的 chunk 化 spec を追加 (= `text:` の透過性を保ちつつ便利機能を別 spec で提供)。

```
hyoui input "$SESS" "chunk-text:$(cat large.txt)" "key:Enter"
```

懸念:
- spec 数が増える
- ICANON 検知は client 側でできず、daemon 側で hint が必要

### 案 C: `MASTER_WRITE_IDLE_TIMEOUT_MS` を引き上げ

500 ms → 2-3 sec に引き上げ、ICANON buffer 飽和時の poll(POLLOUT) 待ち余裕を増やす。

懸念:
- DR-0016 で 500 ms と決めた根拠 (= slow-reader DoS 防止) を後退させる
- buffer 飽和は時間で解決しない場合がある (= 子が永遠に読まないケース)

### 案 D: MANUAL に制約明記のみ、code 変更なし

「ICANON apps に 1 KB 超を送るな、改行で分けて複数 spec にせよ」と仕様化する (= 既に DR-0021 完了時 MANUAL 更新で対応済の想定)。

最小コスト、原則的に正しいが、ユーザビリティは現状維持。

## 推奨 (要 kawaz 判断)

短期: 案 D で MANUAL 明記
中期: 案 A の auto-chunk (= AI agent が意識せず送れる UX)。閾値や `\n` 区切りで悩む部分は実機マトリクスで詰める

## TODO

- [ ] 短期: MANUAL-ja / MANUAL の input 章に ICANON 制約注記 (= DR-0021 統合 MANUAL 更新でカバー予定)
- [ ] 中期: 案 A / B / C のどれを採るか kawaz 判断
- [ ] 採用案の DR 起票 + 実装
- [ ] 解決時は本 issue file を delete + journal/DR に昇華
