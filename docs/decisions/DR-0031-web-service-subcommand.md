# DR-0031: `hyoui web service` — launchd / systemd user の統一自動起動管理

- Status: Active
- Date: 2026-07-29
- Related: DR-0005 (外側自動操作), DR-0014 (介入 self-check), DR-0027 (Web gateway)
- Origin: `docs/issue/2026-07-29-request-web-autostart-service.md` AS-Q1=b

## Context

`hyoui web` を OS ログイン時に自動起動し、異常終了後も復帰させる製品機能が必要。
Homebrew formula に閉じず、macOS LaunchAgent と Linux `systemd --user` を同じ CLI で
管理する。

## 介入判断 self-check

- PTY / child / signal / protocol への介入はない。OS service manager に gateway process の
  起動を依頼する独立した運用層であり、DR-0005 / DR-0014 の透過原則を変更しない
- 新 protocol message / cap flag / daemon state は追加しない
- OS 標準機能 (`launchctl` / `systemctl --user`) を shell-out で利用し、service manager を
  再実装しない

## Decision

### 1. CLI

```text
hyoui web service register [--listen=<host:port>]
hyoui web service unregister
hyoui web service status
```

- `register`: 定義を書き、既存登録を置換し、enable + start する。再実行可能
- `unregister`: stop + disable/unload + 定義削除。未登録なら成功する
- `status`: label / registered / running / pid / definition path を共通形式で表示する
- `hyoui web` の引数なし実行は gateway foreground 起動のまま維持する
- `hyoui web service` の引数なし実行は子 subcommand 一覧を表示する

### 2. OS 対応

| 意味 | macOS LaunchAgent | Linux systemd user |
|---|---|---|
| label / unit | `com.github.kawaz.hyoui-web` | `hyoui-web.service` |
| 定義 path | `~/Library/LaunchAgents/<label>.plist` | `$XDG_CONFIG_HOME/systemd/user/<unit>` (`~/.config` fallback) |
| 起動 command | `ProgramArguments` = `<stable-path> web [--listen ...]` | `ExecStart=` = 同じ argv |
| login 時起動 | `RunAtLoad=true` | `WantedBy=default.target` + enable |
| 継続起動 | `KeepAlive=true` | `Restart=always` |
| 環境 | 最小 `PATH` のみ | 最小 `PATH` のみ |
| log | `~/Library/Logs/hyoui-web/output.log` (stdout/stderr 共通) | journald |

macOS `register` は同 label の既存 job を `bootout gui/$UID/<label>` してから plist を
置換し、`bootstrap gui/$UID <plist>` する。これにより手書き plist を含む既存登録を
冪等に移行できる。Linux は定義更新後に `daemon-reload` → `enable` → `restart` する。

### 3. 安定 executable path

`current_exe()` を直接焼き込まず、`stable-which` の
`resolve_stable_path(current_exe, ScoringPolicy::SameBinary)` で同一 binary を指す安定な
PATH symlink (例: `/opt/homebrew/bin/hyoui`) を選ぶ。dev build / versioned install しか
見つからない場合は登録を止めず、その path が rebuild / upgrade で壊れ得ることを stderr
へ warning する。

### 4. 実装境界

- `ServiceDefinition`: label / program args / env / log path /
  associated bundle identifiers を持つ OS 中立記述
- `render_launchd_plist` / `render_systemd_unit`: 全 platform で compile する純関数
- `Backend`: definition path / register / unregister / status を抽象化し、live shell-out だけ
  `cfg(target_os)` で分離
- parser / help / bash・zsh・fish completion は `hyoui::cli` の command tree と同期する

### 5. renderer を手書き template にする理由

plist / unit は固定された小さな形で、必要な escaping も XML text と systemd quoted value
に限定される。XML / ini crate を追加するより、純関数 renderer + golden test で byte 単位の
監査可能性を持たせる方が依存面・出力安定性の両方で適する。systemd の `%` specifier、改行、
quote、backslash と XML の `&<>` は renderer 境界で escape する。

## Verification

- parser: gateway 既存形 / service parent help / 3 leaf / register `--listen` / 不正引数
- pure unit: label・定義 path、ServiceDefinition argv/env、launchd/systemd golden、escaping、
  launchctl pid parse、status render
- CLI E2E: 隔離 HOME で未登録 status と全 help topic
- macOS dogfooding: `register` → `status` → `launchctl print` → HTTP 200 を確認し、登録状態を残す
- quality gate: `cargo fmt --check`、workspace all-target clippy、関連 test

## Consequences

- OS 起動時の Web gateway 常駐が Homebrew に依存せず再現可能になる
- service 定義には secret や shell session 固有環境を含めず、最小 PATH だけを固定する
- Linux では user manager の lifecycle (logout 後の継続には linger が関係する) は OS 運用側の
  責務として維持する
