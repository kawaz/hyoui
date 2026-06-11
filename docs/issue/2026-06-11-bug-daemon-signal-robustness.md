# bug: daemon のシグナル堅牢性 — graceful shutdown 不在 / SIGCONT 連動不発 / 即死時残骸

- Date: 2026-06-11
- Status: open
- Priority: 中-高 (= 子巻き添え死は構造的事実として DR-0017 に記録済み。本 issue は緩和策の検討と個別 bug の修正)
- Origin: docs/findings/2026-06-11-signal-suspend-interaction-audit.md (2 系統監査 + 実機検証)

## 現象 (実機確認済み、hyoui 0.6.1 / macOS 26.5)

1. **daemon に SIGTERM/SIGINT/SIGHUP handler が無く即死する**。finalize escalation /
   `SessionExitNotify` / socket unlink が全て走らない。OS shutdown / systemd 停止でも同様。
2. **daemon 死亡で child が SIGHUP 巻き添え死する** (anchor 構造の必然、DR-0017 Consequences
   に記録済み)。daemon の即死がそのまま child の死を意味するため、(1) の graceful shutdown
   不在の影響が大きい。
3. **daemon SIGCONT → stopped child の連動起こしが不発** (session.rs:795-803 の防衛策コードが
   実機で発火しない)。child を STOP → daemon に CONT、daemon 自体を STOP→CONT、どちらでも
   child は止まったまま。self-pipe → drain → killpg 経路のどこかで切れている疑い。
4. **daemon 即死時に socket file が残骸として残る** (`list --prune-stale` で掃除可能だが、
   stale socket が一時的に list を汚す)。
5. **daemon 死亡時の attach client が exit 0 で終わる** (`SessionExitNotify` 無しの socket EOF
   を正常扱い)。スクリプトから「子が code 0 で終了」と「daemon が落ちた」を区別できない。

## 修正候補

- (1)(4): SIGTERM を self-pipe に追加し finalize 経路 (escalation + ExitNotify + unlink) へ
  流す。`handle_suspend_signals` の戻り値 `Option<RelayOutcome>` の doc コメント
  (session.rs:770-771) が「将来 SIGTERM 経路を統合した場合の余地」としてこの用途を予約済み。
- (3): 経路を観測して切れている箇所を特定 (SIGCONT handler の self-pipe write → drain →
  waitpid(WCONTINUED) → killpg)。マトリクス検証してから修正 (DR-0014)。
- (5): daemon 死 (ExitNotify 無し EOF) を非 0 exit + stderr 一行に変更 (breaking OK)。
- (2) の根本対策 (= daemon 死でも child を守る) は anchor 構造と排他なので**やらない**
  (DR-0017 の構造判断)。(1) の graceful shutdown で「意図的停止時は秩序立てて畳む」に留める。

## TODO

- [ ] SIGTERM graceful shutdown (finalize 経路統合)
- [ ] SIGCONT 連動不発の経路調査 + 修正
- [ ] daemon 死亡時の client exit code 非 0 化
- [ ] SIGTSTP 吸い込みコメントの実装整合 (handler 登録をやめるか、コメントを実態に合わせる)

## 関連

- [[DR-0017]] (anchor 構造 + 巻き添え死の記録) / [[DR-0014]] (検証主義)
- docs/findings/2026-06-11-signal-suspend-interaction-audit.md §実機検証結果
