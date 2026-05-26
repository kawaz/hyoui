# 孫プロセスの orphan 化検出と対処

> Status: Active
> Date: 2026-05-27
> Related: [[R5-H7]] (killpg 化)、[[DR-0003]] (forkpty + login_tty)

## 症状

- daemon を終了したのに、child の更に子 (= 孫プロセス) が残っている
- `ps -ef --forest` (Linux) / `ps -ef` + `pstree` (macOS) で「親 PID が
  init (= 1)」になっているプロセスを発見
- session 配下で起動した shell が `nohup` / `setsid` / `disown` 等で
  pgid を切り替えていた場合 (= 意図的 detach はこの範疇)
- session 配下の shell で `&` でバックグラウンド起動した長時間 job が、
  parent (= shell) が死んでも生き残っている

## 切り分け

1. session の child PID と pgid を確認:
   ```bash
   # daemon が記録している child PID (status 経由で取得できる場合)
   hyoui status <session>      # child_pid 列
   # pgid を確認
   ps -o pid,pgid,ppid,args -p <child_pid>
   ```
2. **同じ pgid を共有するプロセス**を列挙 (= killpg の対象範囲):
   ```bash
   ps -e -o pid,pgid,ppid,args | awk -v g=<pgid> '$2 == g'
   ```
   `<pgid>` 配下の全プロセスが daemon 終了時に killpg(SIGTERM → SIGKILL)
   で道連れになる (= R5-H7 対応済)
3. **pgid 外に出たプロセス**を探す (= 真の orphan):
   ```bash
   # ppid=1 で hyoui 起源かどうか怪しいものを列挙
   ps -e -o pid,ppid,pgid,args | awk '$2 == 1 && /<command-name>/'
   ```
   `setsid` / `daemon()` 等で **意図的に pgid を切り替えた**プロセスは
   killpg の射程外。これは hyoui の仕様上 detect 不能 (= 子供の意思を
   尊重する)

## 対処

1. **意図しない orphan** (= pgid が切り替わってない、ppid=1 になった
   だけのもの) は通常 R5-H7 解消後は発生しない。発生したら bug:
   - 再現手順を `docs/issue/` に起票
   - `Session::Drop` で `killpg(-pgid, SIGTERM)` → 500ms 待ち →
     `killpg(-pgid, SIGKILL)` が走っているか確認
2. **意図的 detach** (= `nohup` / `setsid` / 自前 `daemon()`):
   - 仕様。手動で `kill <pid>` するしかない
   - tooling として `ps` で PID を引いてユーザに知らせる runbook を整備
3. **session を残したまま** orphan だけ殺す:
   ```bash
   kill <orphan_pid>            # SIGTERM (graceful)
   sleep 1
   kill -KILL <orphan_pid>      # しぶとければ SIGKILL
   ```

## 予防

- daemon 終了時の cleanup は `Session::Drop` の `killpg` に任せる
  (= R5-H7 対応で個別 PID kill から pgid 全体 kill に変更済)
- session 配下で `setsid` / `nohup` / 独自 `daemon()` を使うユーザには
  「detach した process は hyoui の制御外」と仕様明記する (= DESIGN.md /
  MANUAL.md で謳う)
- テストカバレッジ: `crates/hyoui/src/daemon/session.rs:1045-` の
  `session_drop_kills_grandchild_via_killpg` が孫殺しの invariant を
  検証している。新規 session module 改修時はこのテストを壊さない
- v0.2.0+ で serve gateway を入れる際は、HTTP 経由で session を spawn
  する場合の orphan policy を DR で固める

## 関連

- [[R5-H7]] — `Session::Drop` を `killpg` 化した経緯
- [[DR-0003]] — forkpty + login_tty で child を pty 上に置く設計
- [[DR-0009]] — daemon/session.rs の module 分割
- `crates/hyoui/src/daemon/session.rs:52` — `killpg(2)` の使い方
  コメント
- `crates/hyoui/src/daemon/session.rs:1045-1115` — killpg 化テスト
