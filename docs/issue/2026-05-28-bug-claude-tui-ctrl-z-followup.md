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

---

## 調査結果 2026-06-10 (= 「未確認事項」を実機で確定、想定パターン表の答え)

> 検証者: Claude subagent (= 実機マトリクス検証)。バイナリ v0.2.6、Rust 未変更。
> claude TUI 実機は使わず、SIG_DFL の代役 (cat/less/vim/python/bash) で
> 「子が STOPPED するか」を直接観測 (= CLAUDE.md 検証主義、最低 3 カテゴリ)。

### 結論: 想定パターン表の `T` でも `S` でもなく **「SIGTSTP が orphan group で
discard され子が止まらない」が真因** (= 本質は claude 非依存の hyoui 構造問題)

本 issue の「未確認事項 2 (= claude が SIGTSTP catch & continue?)」は **誤った前提**
だった。claude が catch しているのではなく、**hyoui 構造上、子 (= claude を含む全 app)
の process group が orphan になり、line discipline が生成した SIGTSTP を kernel が
discard する**。claude が「suspended」message を出すのは claude 自身の handler だが、
実際の STOP は orphan discard で起きない。

→ 詳細・観測マトリクス・修正方針は姉妹 issue **`2026-05-29-bug-claude-tui-ctrl-z-not-stopping.md`
の「調査結果 2026-06-10」に集約**。本 issue (= claude 限定の疑い) はその一般形 (=
全 TUI app で同症状) に吸収される。

### 想定パターン表への回答 (= 実測)

| stat | 当初解釈 | 実測 |
|---|---|---|
| `T` | hyoui follow trigger 不在 | ✗ 子は T にならない |
| `S` | claude が SIGTSTP catch & continue | △ 見かけは S だが理由が違う。catch でなく **orphan discard** |
| `Ts+` | STOPPED + leader | ✗ 内側 Ctrl-Z / 外部 SIGTSTP では到達不能。外部 SIGSTOP でのみ `Ts+` 到達 |

実測の核: 子は常に `Ss+` (= 止まらず session leader)。外部 `kill -STOP` を送った時だけ
`Ts+` に止まる (= SIGSTOP は orphan でも discard 不可)。これが claude を含む全 app 共通。

### claude 特有事項 (= 残る未確認、ただし優先度低)

- claude の「Claude Code has been suspended」message は claude 自身の SIGTSTP handler
  由来。orphan discard で実際の STOP は起きないので **message と実態が乖離** する
  (= ユーザ期待「suspend したはず」vs 実態「走り続けている」)。これは hyoui 修正
  (= 姉妹 issue 案 A4 で子に SIGSTOP を確実に届ける) で解消する見込み
- claude 実機での `ps stat` 確認は未実施 (= 代役 5 種で一般法則を確定したため不要と
  判断)。必要なら kawaz 正規 path で `ps -o stat -p <claude pid>` が `Ss+` を返すか
  1 点確認すれば足りる
