# bug: claude TUI Ctrl-Z 押下時の真の挙動が未確認

- Date: 2026-05-28
- Priority: 中 (= 再現手順あり、kawaz 連携で確認可能)
- Status: 未着手 (= kawaz の正規 path でのみ再現する破壊的観察を避ける)

## 現象

claude TUI で Ctrl-Z 押下:

- claude TUI が「Claude Code has been suspended. Run `fg`...」message を表示
- しかし **hyoui-cli が follow で STOPPED にならない、外側 shell に戻らない**

一方、harness test (= `/bin/sh -c 'kill -TSTP $$'`) では follow 動作確認済。
→ 「**何かが違う**」が真因不明。

## 未確認事項

1. **claude TUI が実際に STOPPED してるか?**
   - `ps -p <claude pid> -o pid,ppid,pgid,sess,stat,command` で stat を確認
   - stat = `T` (= STOPPED) なら → hyoui の軸 1 follow trigger が来ない bug
   - stat = `S` (= 止まってない) なら → claude TUI が `raise(SIGTSTP)` してない、または catch して continue 処理してる
2. **claude TUI の SIGTSTP 挙動**
   - claude (Anthropic 公式 CLI) が SIGTSTP を **catch して message 表示だけして continue** している可能性
   - その場合 hyoui からは「子が STOPPED してない」ように見えるので follow trigger も来ない (= 正しい挙動)
   - → 「Ctrl-Z で suspend するつもり」のユーザ期待 vs 「claude が SIGTSTP を吸収」の食い違い

## 再現手順 (= 新 session で kawaz と Claude が連携)

```bash
# 1. kawaz: clean state で起動 (= cwd は影響あり、適当な dir)
cd /tmp
SID=$(uuidgen)
hyoui run -- /Users/kawaz/.local/bin/claude --session-id "$SID"

# 2. kawaz: claude TUI で Ctrl-Z 押す

# 3. Claude (= 私): 同時に SID で ps 観測
CLAUDE_PID=$(pgrep -f "$SID" | tail -1)
HYOUI_PID=$(pgrep -f "hyoui run.*$SID" | head -1)
ps -p "$CLAUDE_PID,$HYOUI_PID" -o pid,ppid,pgid,sess,stat,command
```

## 重要 — 観察と破壊的操作の区別

**私 (= Claude) は kawaz の active terminal プロセスに外部 signal を送らない** (= `kill -TSTP` /
`kill -CONT` は cmux freeze 誘発する破壊的操作、本 session で実証済)。

kawaz の **正規 path** (= 手元 Ctrl-Z) でのみ再現する。私の役割は `ps` 観測のみ。

これは DR-0014 §self-check + Anti-pattern の追加候補 (= Issue 5):
- 「観察と称して active session に破壊的 signal を送らない」を明文化すべき

## 想定される結論パターン

| stat | 解釈 | 対応 |
|---|---|---|
| `T` | hyoui の follow trigger 不在 | hyoui の SIGCHLD/waitpid 経路を調査、修正 |
| `S` | claude が SIGTSTP catch & continue | hyoui バグではない、ドキュメント整備 (= claude TUI の挙動として記録) |
| `Ts+` | STOPPED + session leader | 軸 2 transparent は動いてる、軸 1 follow が遅延? |

## 関連

- `docs/issue/2026-05-27-claude-tui-poc-followup.md` (= claude TUI PoC follow-up、Phase C scrollback で部分解決済)
- DR-0001 軸 1 follow (= 親 STOPPED 検知 → follow / decouple 判断)
- Issue 1 (= termios) / Issue 2 (= SIGCONT) と独立だが、同じ handler 周辺を触る
