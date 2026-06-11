# cmux 端末から hyoui run -- claude すると CLAUDE_CONFIG_DIR が失われる (cmux 側の問題)

> 調査日: 2026-06-11。dogfooding 初日に発見。**hyoui のバグではない** (environ の
> 受け渡しは正常と検証済み) が、「cmux 内から hyoui で claude を起動する」は主要
> ユースケースのため調査記録を残す。

## 判明した事実

1. cmux (libghostty ベースの端末アプリ) は shell integration で `claude` を **zsh 関数 →
   バンドル wrapper (`/Applications/cmux.app/Contents/Resources/bin/claude`)** に差し替え、
   さらに PATH 先頭にも同 wrapper を置く (= execvp 経由でも wrapper が起動する)
2. wrapper は `CMUX_SURFACE_ID` が environ にあると `IN_CMUX=1` で cmux 連携モードに
   入り、`NODE_OPTIONS` に guard JS を注入して claude プロセスを surface に紐付ける
3. cmux 端末から `hyoui run -- claude` すると、`CMUX_SURFACE_ID` が hyoui daemon の
   子まで継承される → wrapper は連携モードに入るが、**hyoui daemon の子は cmux から
   見てどの surface にも属さない**ため、アカウント解決が壊れて `CLAUDE_CONFIG_DIR` が
   失われる → 素の `~/.claude` (kawaz 環境では walk-up 対策の regular file) に落ちて
   `ENOTDIR` の Settings Error
4. **hyoui の environ 受け渡しは正常**: `hyoui run -- /bin/sh -c 'echo $CLAUDE_CONFIG_DIR'`
   は正しい値を出す。直接実行との environ diff も HYOUI_NAMESPACE 追加 / OLDPWD /
   SHLVL のみ (2026-06-11 検証)
5. `CMUX_SURFACE_ID` を unset すれば正常起動する (実機確認済み):
   `hyoui run -- /bin/sh -c 'unset CMUX_SURFACE_ID; exec claude'`

## 実用的な示唆

- 運用回避: `hyoui run -- env -u CMUX_SURFACE_ID claude` (zsh 関数化推奨)
- 本筋: cmux 側の修正 — wrapper / guard が「CMUX_SURFACE_ID はあるが自プロセスは
  その surface の直接の子孫でない」ケースを検出して passthrough に落とすべき
- hyoui 側は透過原則に従い何もしない (= env を触る機能は env(1) で代替可能)
- 同種の問題は tmux / screen / nohup 等「surface の environ を引き継いだ別プロセス
  ツリー」全般で起きるはずで、hyoui 固有ではない

## 検証の詳細

| 実験 | 結果 |
|---|---|
| `hyoui run -- /bin/sh -c 'echo $CLAUDE_CONFIG_DIR; which claude'` (cmux 端末) | 値は正しく届く / claude = cmux wrapper |
| 直接実行と hyoui 経由の environ diff (Claude Code セッション環境) | HYOUI_NAMESPACE / OLDPWD / SHLVL のみ |
| hyoui 経由 claude 起動 (CC セッション環境 = CMUX_SURFACE_ID なし) | Settings Error なし (素の claude / cmux wrapper 経由の両方) |
| `hyoui run -- claude` (kawaz の cmux 端末 = CMUX_SURFACE_ID あり) | Settings Error (ENOTDIR ~/.claude) |
| `hyoui run -- /bin/sh -c 'unset CMUX_SURFACE_ID; exec claude'` (同) | **正常起動** (個人面 config で動作確認) |

wrapper の env 操作箇所 (調査時点): CLAUDE_CONFIG_DIR の直接操作は legacy パス
normalize のみ。auth selection unset 対象は ANTHROPIC_API_KEY 等で CLAUDE_CONFIG_DIR を
含まない。失われる正確な機序は guard JS (NODE_OPTIONS 注入) 側と推定 (cmux 内部、未追跡)。

## 追記 (2026-06-11 夜): 自然解消

同日夜、`hyoui run -- claude` (回避なし) が正常起動するようになった (Settings Error /
FleetView とも出ない)。間に変化した外部状態:
- cmux のアカウント切替 (前日リミットで一時切替 → 復帰) の状態変化
- claude 2.1.172 → 2.1.173 自動更新

決定的な再現条件は特定できず。**「cmux の auth selection が切替中の状態 × surface に
属さないプロセスからの wrapper 起動」の複合条件**と推定。再発時は「直前に cmux の
アカウント切替をしたか」を確認すること。

なお途中で観測された FleetView 起動 (= `env -u CMUX_SURFACE_ID` 回避時) は、当時
走っていた hyoui 経由の claude job 群が存在したためで、終了後は出ない。

## 追記 2 (2026-06-11): 再現条件がほぼ確定 — OS 再起動による stale surface

kawaz の追加情報で再現条件が判明:
- 途中で **mac を再起動**しており、cmux は再起動後に surface 群を復元していた
  (一部は claude 起動状態まで復元、復元しきれない surface も多数)
- **Settings Error が出たのは再起動前から開きっぱなしの古い surface**、
  正常起動したのは**新規ワークスペース/新規タブ** (fresh な surface)

### 確定した機序

OS 再起動で復元された surface の environ にある `CMUX_SURFACE_ID` は**前世代の
stale ID**。その環境から起動した wrapper は stale ID で IN_CMUX=1 となり、
cmux socket への auth selection 問い合わせが「unknown surface」となって
CLAUDE_CONFIG_DIR が失われる。新規 surface (fresh ID) では起きない。

wrapper には「socket unavailable → passthrough」の stale 対策が既にあるが、
「**socket はあるが surface ID が unknown**」のケースが穴。

### cmux への報告事項 (要約)

OS 再起動で復元された surface の stale `CMUX_SURFACE_ID` を引き継いだプロセス
(hyoui daemon の子に限らず、その surface のシェルからの起動全般で起きうる) から
claude wrapper を起動すると CLAUDE_CONFIG_DIR が失われる。unknown-surface 応答も
socket-unavailable と同様に passthrough へフォールバックすべき。
