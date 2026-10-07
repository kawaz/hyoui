---
title: session id を UUID に、namespace を廃止して tag に、socket をフラットに置く (DR-0018 を置き換える)
status: resolved
category: design
created: 2026-10-04T15:30:00+09:00
last_read: 2026-10-07T11:00:00+09:00
open_entered: 2026-10-04T15:30:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered: 2026-10-07T11:00:00+09:00
discard_reason:
pending_reason:
close_reason: DR-0041 として実装 (session id は UUID、namespace を廃止して key=value の tag、sessions/ にフラット、面は HYOUI_STATE_DIR の 3 段、長い sun_path は fork + fchdir)。namespace の option の受け付けは remove-namespace-option-shim で 2026-11 に消す
blocked_by:
---

# session id を UUID に、namespace を廃止して tag に、socket をフラットに置く

議論フェーズの記録 (2026-10-04)。DR-0018 (namespace) を置き換える DR の素材。発端は web の状態の置き場 (`hyoui/web/`) が session socket の木 (base 直下のサブ dir = namespace) と衝突した件 (`docs/QUESTIONS.md` の WR-Q1、`docs/issue/2026-10-04-web-unit-registry-holds-settings.md`)。

## 合意

### namespace を廃止して tag にする

- namespace は「名前を付ける」と「見えなくする」を 1 つの仕組みに同居させており、後者が「list に出ない」「ns 指定が漏れて操作できない」という UX の悪さを生んでいる。session id は元々一意なので階層化は要らない
- 分類とフィルタは tag (session のメタデータ) で行い、既定は全部見える
- DR-0018 が tag 案 (方式 (c)) を退けた理由は (1) 旧 daemon との互換処理 (2) 全 socket に問い合わせた後の絞り込みで I/O が残る、の 2 つ。(1) は v1.0 前で互換を持たない方針なので当たらない、(2) は list が元々全件に問い合わせる作りで件数も数十なので実害が小さい

### session id は UUID だけ

- 自動の id (`run-<pid>-<hex>`) と自分で付ける id の混在をやめ、UUID に揃える。起動側が `--session-id <UUID>` で最初から指定でき (起動後に stdout から id を読まずに済む)、指定が無ければ自動で振る
- UUID の版は規定しない (外部指定のユースケースがある以上意味がない)。list の並び順は started_at で決め、id の版に依存しない
- 人が打つための短縮指定 (先頭一致等) は入れない (コピペ前提)
- 同じ id の socket が既にあれば run 自体をエラーにする。古い socket が残っている (daemon は死んでいる) 場合も重複として扱う。死んだ socket の片付けは片付けの経路の責務で、run は生死を判定しない (判定経路を 2 つに分けない)。エラーには原因と対処 (別 id で起動 / 片付けてから再実行、片付けのコマンド名) を書く
- 重複の判定は bind / lock の時点で原子的に失敗させる (確認してから作るの 2 段にしない)
- 子に注入する `HYOUI_SESSION` (DR-0020) は値が UUID になるだけ

### socket をフラットに置く

- session の socket は `hyoui/sessions/<uuid>.sock` に置く。`hyoui/` 直下は機能別のサブ dir (`sessions/`、`web/` ...) だけにする。これで web の状態の置き場 (`hyoui/web/`) と衝突しない (WR-Q1 の解消)
- unix socket の長さの上限は bind / connect に渡す `sun_path` 引数の長さであって、ファイルシステム上のフルパスの長さではない。フルパスの長さで弾かない (現行の `check_sun_path_len` はフルパスで弾いており誤り)。相対パスを渡して開く。chdir はプロセス全体に効くので、マルチスレッドのプロセス (gateway の tokio、daemon) では使わない。macOS には dirfd 基準の `bindat` / `connectat` が無い (SDK ヘッダと libsystem_kernel の export で確認、2026-10-04)
  - 推し: 共有 fd + fork した子だけが cwd を変える。親で socket を作ってから fork し、子は `fchdir(dirfd)` → 相対名で `bind` / `connect` → `_exit(結果)`、親は子の終了を待つ。fork 後の fd は親子で同じ open file description を指すので、子の bind / connect は親の fd にそのまま効き、SCM_RIGHTS での受け渡しも要らない。cwd が変わるのは子だけで親のスレッドは巻き込まれない。fork から `_exit` までは async-signal-safe な呼び出し (fchdir / bind / connect / _exit) だけなので、マルチスレッドのプロセスから fork しても POSIX の範囲で安全。fork するのはパスが `sun_path` の上限を超える時だけで、超えなければ今どおり直接開く
  - 退けた候補: macOS の `pthread_fchdir_np` (スレッドごとの cwd) は libsystem_pthread が export しているがヘッダに公開宣言が無い (fts.h に「private」とあるだけ) 非公開 API。Linux の `/proc/self/fd/<dirfd>/<name>` は macOS に無い。短い symlink は置き場の制約 (macOS は `/tmp` を掃除する) と後始末を背負う

### 面の分離 (使うなら)

- 面は状態の root を決める環境変数 1 つで決まる (`CLAUDE_CONFIG_DIR` と同じ考え方)。hyoui の一式 (CLI、session の socket、web の監督者・unit・登録簿・auth.json・logs) はその root の中で完結する。tag は分類で、認証境界の分離には使わない
- 面をまたぐ仕組み (複数の面を横断する option、1 つの監督者で複数の面の unit を抱える等) は持たない。複数の面を扱う時は面ごとに環境変数を指定してそれぞれで実行して回る
- web の監督者も面ごとに立つ: 面の `.envrc` が効いた状態で `hyoui web service register` すれば、その面の root が plist に固定される (DR-0038 の env 固定)
- 面ごとの監督者の label (合意 2026-10-04): `com.github.kawaz.hyoui.web.supervise.<hash>`。`<hash>` は状態の root を realpath で正規化した絶対パスの hash の先頭 8 桁。既定の面も含め常に付け、既定か否かの判定は持たない。label は人が意識しない (CLI のサブコマンド経由で操作する) ので、`service status` に label と固定した env (root) を並べれば足り、登録の列挙は label の接頭辞で引ける。DR-0038 で実装

## 未決

- 現行の `HYOUI_NAMESPACE` を使っている箇所の移行: 業務面の `.envrc`、ccmsg の hyoui terminal 連携 (`src/terminals/hyoui.ts` が base と namespace を直書きで discovery している。ccmsg 側の issue として起票が要る)
- 動いている session の移行 (新旧の hyoui が混在する短い期間の扱い)。方式は合意 (2026-10-04): 置き場を移す時は移動して古い置き場に新しい置き場への symlink を残し、後で必ず消す。古い CLI は symlink をたどって `connect` できる。新しいバイナリは新しい置き場だけを見る (symlink を二重に拾わない)。新しいバイナリは古い置き場の symlink が残っていれば警告する。symlink を消す条件は版で明記する
