# リポジトリ物理構造

> このファイルは hyoui リポジトリの **物理的なファイル / ディレクトリ配置**を俯瞰する。
> 各モジュールの責務・設計判断の詳細は [DESIGN-ja.md](DESIGN-ja.md) / [DESIGN.md](DESIGN.md) と
> [decisions/](decisions/) (DR-NNNN) を参照。

## トップレベル

```
hyoui/
  README.md / README-ja.md   # プロジェクト概要 (翻訳ペア)
  LICENSE                    # MIT
  Cargo.toml                 # Cargo workspace 定義 (version 正本、members = 2 crate)
  Cargo.lock
  deny.toml                  # cargo-deny 設定 (license / advisory / ban)
  justfile                   # task runner (canonical = kawaz/bump-semver、push task で check/test/bump)
  CLAUDE.md                  # Claude Code 用プロジェクトルール (必読 DR 一覧 + self-check)
  crates/                    # Rust ソース (2 crate)
  docs/                      # 設計・運用・履歴 (本ファイル群、docs-structure 準拠)
  *.cast                     # asciinema 録画 (README デモ素材)
```

リリース履歴の正本は [GitHub Releases](https://github.com/kawaz/hyoui/releases) と git/jj log
(CHANGELOG ファイルは持たない、kawaz/bump-semver の方針に統一)。

## crates/ (Cargo workspace)

2 crate 構成。core 機能は library crate に集約し、CLI は薄い binary crate。

```
crates/
  hyoui/             # library crate (= 全コア機能、crate 名 hyoui / lib 名 hyoui)
    Cargo.toml
    src/
      lib.rs         # re-export
      cli.rs         # CLI parser + 各 subcommand 定義
      scrollback.rs  # byte-base ring buffer (tail の since/last_bytes 用)
      strip.rs       # ANSI escape sequence strip (tail --strip 用)
      daemon/        # daemon (= session 1 つを抱える server)
        mod.rs
        config.rs    # session config (socket path, scrollback size, screen sizes)
        session.rs   # Session::serve = multi-attach + broadcast + control plane
        control.rs   # control message dispatcher
        broadcast.rs # writer pump + backpressure + ClientHandle
        accept.rs    # handshake worker pool
        wait.rs      # state polling 補助 (snapshot 発火 trigger / poll interval)
        tail.rs      # tail subscription
        lock.rs      # SessionState + leader cascade
        pty.rs       # child lifecycle
        record.rs    # tty I/O timeline 録画 (DR-0016)
        screen/      # vt100 ScreenState wrapper (DR-0013)
          mod.rs
          state.rs       # VirtualScreen (vt100::Parser を抱える正本)
          input_log.rs   # primary 用 bounded ring (resize replay)
          snapshot.rs    # 構造化 snapshot wrapper (CBOR 圧縮)
          redraw.rs      # attach 時の初期 redraw
          health.rs      # screen state health 判定
      client/        # client (= daemon に attach する側)
        mod.rs
        attach.rs    # ClientConnection (handshake + raw I/O + detach prefix + raw bytes 送信)
      protocol/      # wire protocol
        mod.rs
        frame.rs     # u32 size + u8 type + body の framing
        caps.rs      # capability negotiation (MVP_CAPS, intersect)
        messages/    # CBOR control message types
          mod.rs
          handshake.rs / lifecycle.rs / session_lifecycle.rs
          control.rs / status.rs / lock.rs / tail.rs / screen.rs
          record.rs / error.rs
        transports/  # Transport trait + UnixStreamTransport
          mod.rs / unix.rs
      sys/           # unsafe を集約
        mod.rs
        raw.rs       # forkpty / login_tty (子プロセス起動)
        signal.rs    # sigaction 登録、self-pipe
        pty.rs       # PTY abstraction
        socket.rs    # Unix socket bind (perm 0600 / dir 0700)
        clock.rs     # Instant ↔ epoch ms 変換 (now_unix_ms 等)
        poll.rs      # poll(2) wrapper
        wait.rs      # waitpid wrapper
        fd.rs / env.rs / tty.rs / error.rs
    examples/        # PoC 検証用 (DR / findings の裏取りに対応)
      01-daemon-fork.rs 〜 08-ansi-strip.rs
      README.md      # 各 example の対応 findings 一覧
    tests/
      sys.rs         # sys モジュールの結合テスト

  hyoui-cli/         # binary crate (`hyoui` command)
    Cargo.toml       # [[bin]] name = "hyoui"
    src/
      main.rs        # entry point、cli.rs の Command を dispatch
      daemonize.rs   # double fork + setsid (--detached)
      socket_path.rs # socket dir resolver (XDG runtime / cache fallback)
      input_handlers.rs # input family の subcommand handler
      wait_core.rs   # state-based wait polling (snapshot 発火 + cells → text 構築)
      completion.rs  # shell completion 生成
    tests/           # CLI 統合テスト (PTY 越し E2E)
      smoke_pty.rs / jobcontrol_follow.rs / lock_cli.rs
      record_cli.rs / record_e2e.rs / matrix_attach_restore.rs
      common/        # テストヘルパ (pty.rs / normalize.rs / mod.rs)
      snapshots/     # スナップショットテストの固定データ
```

## docs/ (docs-structure 準拠)

```
docs/
  DESIGN-ja.md / DESIGN.md     # アーキテクチャ設計書 (翻訳ペア)
  STRUCTURE.md                 # 本ファイル (物理構造、翻訳ペア対象外)
  ROADMAP.md                   # scope 正本 (version 区切りは持たない)
  MANUAL-ja.md / MANUAL.md     # ユーザ向け操作マニュアル (翻訳ペア)
  REVIEW-BACKLOG.md            # レビュー指摘の backlog (R*-* ID 管理)
  decisions/                   # DR-NNNN-*.md (設計判断記録) + INDEX.md
  journal/                     # YYYY-MM-DD-<slug>.md (開発ジャーナル、経緯)
  findings/                    # YYYY-MM-DD-<slug>.md (調査の確定事実)
  runbooks/                    # 運用手順 (再発する問題と対処) + INDEX.md
  issue/                       # ローカル issue 起票 (解決時 delete、README.md に運用)
  research/                    # 外部技術の調査メモ (multiplexer / vt100 crate 比較等)
  design/                      # (予約、現状空)
  knowledge/                   # (予約、現状空)
```

履歴の正本は GitHub Releases。経緯は journal、確定事実は findings、設計判断は decisions、
運用手順は runbooks、という棲み分け (`docs-knowledge-flow` ルール)。

## .github/

```
.github/
  workflows/
    ci.yml       # push の lint + test。jobs: test (fmt/clippy/build/test) / ignored-tests
                 #   (PTY+daemon の --ignored 統合テスト) / msrv (rust-version=1.88 check) /
                 #   audit (cargo-audit) / deny (cargo-deny)。ubuntu + macos マトリクス
    release.yml  # Cargo.toml の version 変更を trigger に build → tag → GitHub Release
                 #   (kawaz/bump-semver に version 比較を委譲、4 platform バイナリ + homebrew tap)
```

tag 打ちと GitHub Release 作成は release.yml が自動で行う (人 / Claude は tag を打たない、
`release-flow-awareness` ルール)。
