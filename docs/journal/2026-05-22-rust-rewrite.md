# 2026-05-22 hyoui を Rust に一本化

MoonBit + Rust FFI 二層構成 (poc) を全廃し、Rust 単一実装に切り替えた経緯と方針確定の記録。

## 経緯

2026-05-21 にブートストラップした初期実装 (MoonBit ロジック層 988 行 + Rust FFI 1146 行 + MoonBit FFI バインディング 535 行) は **poc 扱い**で全捨て。poc は「PTY/シグナル/socket/EOF/ジョブ制御の問題が解けるか」の実証実験であり、本実装はゼロから言語選定し直すと判断。poc コードは `bootstrap` bookmark + `../bootstrap/` workspace に保全 (ローカルのみ、origin への push は別途検討)。

## 方針確定 (議論ログの要約、詳細は DR-0003/DR-0004 参照)

### 言語: Rust 一本化 (MoonBit / Go 却下)

hyoui の本質は OS の PTY/シグナル/poll を相手にする薄いシステムプログラム。アルゴリズム重さでも開発速度でもなく、**ランタイムに邪魔されず syscall を精密に叩けるか**が言語選定の主軸。

- **MoonBit 却下**: 結局 FFI 先 (Rust) に syscall を書く二層構造になる。ロジック層 (cli + agent + observer) ですら syscall と密結合するため「MoonBit 側に置いて嬉しいコード」がほぼ無い。FFI の書き方を改善する余地はあったが、「うまく書けるか」以前に「二層にする意味があるか」で No
- **Go 却下**: Go ランタイムは非同期プリエンプションでシグナルを内部利用し、シグナルは必ず `signal.Notify` チャネル経由。DR-0001 の bg/fg ジョブ制御 2 軸 (SIGTSTP/SIGCONT の精密制御) と相性が悪い。raw mode 中の GC/プリエンプションも透過性に乗る。書けはするが Rust より「ランタイムと戦う」場面が増える
- **Rust 採用**: ランタイム介在なしで libc を直接叩ける + `cdylib`/`staticlib` で C ABI を素直に出せる (将来ライブラリ化の自由度)

### crate 構成: lib + bin の最初から分離

将来像 (単体 CLI に留めるか / ライブラリとして組み込むか) が未定。`crates/hyoui` (lib) + `crates/hyoui-cli` (bin、`[[bin]] name = "hyoui"`) で分離しておけば、ライブラリ化が現実味を帯びた時に再構成不要。テストも lib 側に集めやすい。

### 履歴: bootstrap workspace 保全 + main 作り直し

「Initial empty commit」(`pprmuvnu 8c8d27a3`) から `jj new` し直し、保全対象 (DR-0001/0002, INDEX, LICENSE, README, ci-release issue) だけを bootstrap から `jj restore --from bootstrap` で復元するクリーンな作り直し。poc コード参照は `../bootstrap/` workspace を見ればよい。

### syscall 層: nix 採用 + ハイブリッド (posix_spawn 等は libc 直)

「自前で型安全ラッパーを完備し続ける未来」を考えると、拡張のたびに `WaitStatus` 相当の型設計を再発明することになる (= nix が既に払ったコストの二重払い)。poc 全捨て確定で「既存コードを壊さない」という libc 直の最大の利点が消えたため、ゼロから書くなら nix 優位。

- **nix 主体**: `WaitStatus` (Stopped 検出が DR-0001 の核心とドンピシャ)、`termios` (raw mode)、`Errno::EINTR` (リトライ判定の型安全) などを積極活用
- **libc 直 (nix 外)**: `posix_spawn` 一族・`ioctl(TIOCSWINSZ/TIOCGWINSZ)` 等。nix と libc 併用は普通の構成
- **unsafe 封じ込め**: `sys/raw.rs` (子起動・winsize ioctl) と `sys/signal.rs` (sigaction 登録・self-pipe) の 2 ファイル限定。lib crate 残部は nix 安全 API のみ、bin crate は `#![forbid(unsafe_code)]`

### 子プロセス起動: forkpty + login_tty (poc の posix_spawn から戻し)

`posix_spawn + SETSID` では **controlling terminal (ctty) が取れない**。`POSIX_SPAWN_SETSID` で session leader にはなるが、**macOS は `TIOCSCTTY` 明示が必須**で、setsid 後に slave を dup するだけでは ctty 化しない。ctty が無いと Ctrl-Z 由来の SIGTSTP が子に届かず、DR-0001 のジョブ制御 2 軸が実用にならない。

poc が posix_spawn を選んだ理由「MoonBit の fork は GC で危険」は **Rust 化で消滅**。`forkpty` は親で `openpty` + `fork`、子側で `login_tty(slave)` を自動実行し、`setsid + TIOCSCTTY + dup2(slave, 0/1/2)` を一括で行う。子側でやることは `login_tty` → `execvp` のみで、両者とも async-signal-safe なので fork 制約に違反しない。

### CLI: サブコマンド方式 (`hyoui run -- cmd [args..]`)

`hyoui -- cmd` のフラット構造では将来 `send`/`attach`/`status` (socket 経由制御) を足す余地がない。最初からサブコマンド方式 (`cli-design-preferences.md` 準拠)。サブコマンド省略・引数なしは常に `--help`。初期実装は `run` + `completion` のみ、残りは枠予約。

`run` を採用 (`exec` 不採用): `exec` はシェル組み込み (プロセス置換) や `docker exec` (既存内実行) の語感。hyoui は新規 spawn なので `docker run`/`cargo run`/`gh run` 系の **`run`** が素直。

## jj 操作のハマり所 → 解決

| 詰まり | 解決 |
|---|---|
| `jj bookmark set main -r pprmuvnu` が `Refusing to move bookmark backwards or sideways` で拒否 | 意図的な backwards (rewrite) なので `--allow-backwards` 付与。jj-tips.md の警告は「別ブランチのマージを失う」リスクのため。今回は bootstrap で保全済みで該当しない |
| `pkf run push` が main 専用ハードコード (`jj bookmark set main -r @-; jj git push --bookmark main`) で bootstrap を push できない | 現 Taskfile は段階 6 で全廃予定。bootstrap origin push はワークフロー検討後に手動で実施 (ローカル bookmark として保全継続) |
| `jj new pprmuvnu` 後に `_build/`/`ffi/target/` の 1MB 超ファイルが snapshot 拒否 warning | jj 追跡外なので物理削除 (`rm -rf`) で対処 |
| **pkf run push の push task が `jj bookmark set main -r @-; jj git push --bookmark main` 仕様** → 段階 0 序盤に bootstrap 保全のため `pkf run push` を試した時、当時の @ = 段階 0 復元 commit、その親 `@-` = pprmuvnu (Initial empty commit) → main bookmark が pprmuvnu に強制 set されて push → **remote main が wkmvsqrq (poc) から pprmuvnu に巻き戻った** (poc は GitHub reflog で 90 日復旧可能) | 段階 6 完了後の push 時は `jj new` で空 change を作って @ を空にしてから `pkf run push` (= `@-` が wxyrruot を指す状態にしてから push)。これが Taskfile の push task の運用前提 |
| 初回 push で `kawaz.semver.checkBumped` が **「No such path: VERSION」** で失敗 (remote main = pprmuvnu = Initial empty commit には VERSION が無いので bump-semver compare gt が比較不能) | Taskfile.pkl の push deps から `checkVersionBumped` を一時除外して `pkf run push` 実行 → push 完了後 `jj restore --from @- Taskfile.pkl` で取り消し (一時編集は @ にのみ残し commit しない) |

## 結果

- v0.0.0 リリース成功 (https://github.com/kawaz/hyoui/releases/tag/v0.0.0)
- 4 architecture artifact: `hyoui-0.0.0-{aarch64,x86_64}-{apple-darwin,unknown-linux-gnu}.tar.gz`
- CI matrix (ubuntu + macos) と release matrix (4 target) すべて green
- ローカル: 97 件テスト pass、unsafe 封じ込め検証 0 件、`pkf run ci` green

## 残課題

`docs/issue/` に起票済み:
- CI / release ワークフロー整備 (Rust 前提に書き換え済み、本フェーズで実装完了 → close 候補)

未起票:
- **bootstrap origin push**: kawaz が手動 (ワークフロー検討) で実施。それまでローカル `bootstrap` bookmark + `../bootstrap/` workspace で poc 保全継続
- **pkf-tasks / bump-semver の initial-release 対応**: `kawaz.semver.checkBumped` の `bump-semver compare gt` が「compareRef 側に VERSION ファイル不在」で失敗するケースの fallback (新規ファイル = bump 済扱い、または compareRef 検出失敗時の skip 等)。kawaz/pkf-tasks に issue 起票候補
- **GitHub Actions Node.js 20 deprecation**: `actions/{checkout,upload-artifact,download-artifact}` 等が Node.js 20 で動いている (2026-09-16 に runner から削除予定)。Node.js 24 対応版へ更新
- **SIGTSTP の cargo test 環境特有挙動**: 段階 2 の forkpty ctty 検証テストで SIGTSTP が cargo test 環境ではブロックされる現象 → SIGSTOP に切り替えて回避。本番 binary での SIGTSTP delivery 動作確認 (smoke) は未実施。段階 4 の agent イベントループで parent 側 SIGTSTP re-raise 経路を本番で叩く必要が出たら検証

新規 DR (段階 6 で起票済):
- DR-0003 (Rust 一本化と MoonBit 却下、forkpty + login_tty 採用)
- DR-0004 (CLI サブコマンド設計)
