# ccmsg が依存する hyoui 機能面と open issue の仕分け

hyoui の現在の主用途は ccmsg のセッション実行基盤 (README Status 参照)。open issue を「ccmsg が依存する機能面に関わるか」で 2 値に仕分け、hyoui の次の作業単位を絞る材料にする。

## ccmsg が使う hyoui の面 (ccmsg リポのソース・DR を grep した観測)

| 面 | 使い方 | 出典 (claude-ccmsg リポ) |
|---|---|---|
| セッション起動 | `hyoui run --detached -- claude …` | `docs/decisions/DR-0018-session-launcher.md` |
| 入力配送 | `hyoui input --namespace <ns> text:/rename <title>` | `packages/daemon/src/session-rename.ts` |
| web embed | webui の iframe が `<gateway>/sessions/<id>?embed=1&resize=1&fab=…` を直接開く (hyoui の web UI をそのまま埋め込み) | `packages/webui/src/client/terminal-gateway-store.ts`、`packages/daemon/src/config.ts` |
| 環境変数 | 子プロセスの `HYOUI_SESSION_ID` / `HYOUI_NAMESPACE` を `claude agents` 経由で読む | `packages/protocol/src/index.ts`、`packages/daemon/src/session-rename.ts` |

使っていないもの (grep 0 件): `hyoui status` / `hyoui kill` (session_kill は pid へ SIGTERM 直送) / `hyoui attach` / `hyoui wait` / `hyoui dump`。

## 仕分け (2026-09-15 時点の active issue 41 件)

### 該当 (11 件) — ccmsg の run / input / web embed 経路に効く

| slug | status | 経路 |
|---|---|---|
| web-gateway-restart-kill-not-reliable | open | web gateway 自体 |
| attach-osc8-hyperlink-metadata-loss | open | web (CLI attach と共通の復元経路) |
| web-screen-fetch-alt-mode-lost | open | web `/screen` |
| handshake-redraw-deferred-no-timeout | open | web の attach 復元 (DR-0013 §4) |
| web-narrow-symbol-fallback-font | open | web |
| web-ime-safari-ios-unverified | open | web |
| child-spawn-sigttou-stop-race | open | `hyoui run` の fork〜exec race |
| feature-icanon-large-input-chunking | open | `hyoui input` |
| feature-ack-test-coverage-expansion | open | `hyoui input` の ack (DR-0021) |
| socket-dir-tmp-fallback-macos-cleanup | open | socket 消失で run / input / web すべての接続が不能 |
| bug-anchor-startup-sigttin-transient | blocked | `hyoui run` の fork〜exec 起動経路 (DR-0017 session anchor) |

### 非該当 (27 件)

CLI attach 専用 (take-leader / child-suspend-action-menu / attach-overlay-progress / tcsaflush-input-discard / ctrlz-guard: attach client の stdin 経路で web 入力には接続しない)、`hyoui wait` / `dump` / `record` / `tx` 系、`hyoui kill` の挙動 (sigcont-alive-child-session-vanish、feature-signal-ack)、CI flaky 系 5 件 (flaky-serve-propagates-child-exit-code 含む: ccmsg の exit 検知は `claude agents` poll で serve の exit code を使わない)、内部リファクタ・アイデア・README 整備、upgrade-e2e-test。

## 使い方

次の作業単位を選ぶときは「該当」から取る。同日の棚卸しで、該当のうち修正 land 済みで issue が残っていたもの (zero-size 2 件 / font-load-fit-race) は close 済み。残りは全て未着手または実機検証待ち。
