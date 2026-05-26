# Finding: PoC 01-08 統括 — 大きな想定外なし、protocol DR と本実装に進める

- Date: 2026-05-26
- 関連: [[DR-0005]] (思想)、[[DR-0006]] (CLI ground rules)、[[DR-0007]] (MVP scope)
- 個別 findings:
  - [[2026-05-26-daemon-fork]]
  - [[2026-05-26-multi-attach]]
  - [[2026-05-26-paste-and-alt-screen]]
  - [[2026-05-26-fd-passing-vs-stream]]
  - [[2026-05-26-resize-propagation]]
  - [[2026-05-26-lock-token-env]]
  - [[2026-05-26-scrollback-ring-buffer]]
  - [[2026-05-26-ansi-strip]]

## 結論: 設計議論の方向で問題なし、本実装フェーズへ

8 つの PoC を順次実行し、hyoui の主要な技術的不確実性を全てクリア。**大きな想定外なし**、設計議論で固めた CLI ground rules ([[DR-0006]]) と MVP scope ([[DR-0007]]) の方向でそのまま実装可能。

## PoC 結果サマリ

| # | 検証項目 | 結果 | 主な学び |
|---|---|---|---|
| 01 | daemon 化 (double-fork) | ✅ PASS | stdio detach (= /dev/null dup2) が必須、parent shell pipeline 解放のため |
| 02 | multi-attach + broadcast | ✅ PASS | poll で十分、UnixSock::listen は dir 0700 必須、ONLCR で `\n→\r\n` 変換 |
| 03 | bracketed paste + alt screen | ✅ PASS | bash -i で `?2004h`、vi で `?1049h` 確認、観測 byte rate (vi 起動 1.8 KB/s) |
| 04 | SCM_RIGHTS fd passing | ✅ PASS、ただし**不採用** | stream 中継で十分、transport 抽象化のため SCM_RIGHTS 不要 |
| 05 | leader resize 伝播 | ✅ PASS | TIOCSWINSZ で SIGWINCH 確実伝播、bash trap は `read -t` で即発火 (sleep は遅延) |
| 06 | lock token env 継承 | ✅ PASS | Unix env inheritance 素直、孫まで継承、`env_clear` で消える |
| 07 | scrollback ring buffer | ✅ PASS (21/21) | DR 議論ユーザ例 (1MB + 900KB + 2KB×60) 含めて想定通り |
| 08 | ANSI escape strip | ✅ PASS (synthetic 11/11 + 実 sample ESC 残留 0) | 50 行 state machine で CSI/OSC/DCS/single 全 strip |

## 想定外 / 注意事項 (本実装で反映)

### 軽微な実装注意点

1. **`nix::unistd::dup2` の API 変更** (PoC 01b): nix 0.31 は `&mut OwnedFd` 要求、stdin/stdout/stderr の raw fd には不便。**libc::dup2 を直接呼ぶ** (sys/raw.rs に置く)
2. **`nix` crate に `uio` feature がない** (PoC 04): SCM_RIGHTS は libc 直接で書く必要があったが、不採用結論なので追加不要
3. **`UnixSock::listen` の dir 0700 precondition** (PoC 02): 既存実装の安全性チェック。MVP の socket 配置 (`$XDG_RUNTIME_DIR/hyoui/` or `$TMPDIR/hyoui-$UID/`) と整合
4. **pty の ONLCR で `\n→\r\n`** (PoC 02): wait/match の text/pattern で `\r?\n` regex 推奨、または装飾除去で CRLF→LF 正規化 (ただし装飾除去は escape 専門にする方が筋、改行は別 flag が良い)

### 子プロセスの挙動依存

5. **bash の trap 発火タイミング** (PoC 05): `sleep` 中は SIGWINCH の trap 実行が遅延、`read -t` で即発火。**hyoui 側の責務外**、signal を「届ける」だけが daemon の仕事
6. **zsh 起動の重さ** (PoC 03): kawaz の zsh rc が 2 秒で起動完了しない。PoC では bash -i で代替
7. **`bash -i --norc --noprofile` の組み合わせで err** (PoC 03): bash の argv 解釈の癖、`bash -i` 単独で OK。**hyoui 側の責務外**

### DR への反映候補 (本実装フェーズで update)

8. **CRLF→LF 正規化を装飾除去から分離** (PoC 08): [[DR-0006]] §11 で「装飾除去の一部として CRLF→LF」と書いたが、責務分離するため別 flag (`--newline-convert=preserve|lf` 等) が筋。装飾除去は ANSI escape 専門に絞る方が一貫
9. **ANSI strip の C0/C1 取扱を明文化** (PoC 08): BEL/BS/etc. を残す or strip するか doc 明示。MVP default は「ESC 系のみ strip、C0/C1 はそのまま」、`--aggressive` で C0/C1 も strip は v0.3.0+ 候補

これら 2 件は **本 PoC で発見した DR 微修正**、次回 DR update commit に含める想定。

## 設計議論で想定済の事項を実証

| 設計 | PoC で実証 |
|---|---|
| screen 型 (1 daemon 1 子) | PoC 01-02 で動作 |
| 起動 = daemon + 自動 attach | PoC 01 で daemon 化、PoC 02 で attach 動作 |
| 複数 attach + 内部 leader | PoC 02 で broadcast/multiplex |
| TIOCSWINSZ で resize 伝播 | PoC 05 |
| HYOUI_LOCK_TOKEN env 継承 | PoC 06 |
| timestamped chunks + last_evicted_ts | PoC 07 |
| ANSI escape 自動検出 + strip | PoC 03 (検出) + PoC 08 (strip) |
| Unix socket + poll | PoC 02 |
| transport 独立な protocol | PoC 04 (= SCM_RIGHTS 不採用、stream 中継一本化) |

## 本実装への持ち越し

### MVP 実装で直接使える PoC コード

- **`07-scrollback-ring.rs` の `Scrollback` struct** → そのまま `crates/hyoui/src/scrollback.rs` に
- **`08-ansi-strip.rs` の `strip_ansi`** → そのまま `crates/hyoui/src/strip.rs` に
- **`02-multi-attach.rs` の poll loop パターン** → daemon の event loop の雛形に

### 次のステップ (= PoC 後の正規実装)

1. **DR-0008 (protocol design) 起草**: message kinds, wire format (length-prefixed bincode/msgpack/json?), handshake, capability negotiation
2. **DR-0006 微修正 commit**: 装飾除去から CRLF→LF 分離、ANSI strip の C0/C1 取扱明文化
3. **v0.0.1 実装**: daemon 化 + run/attach/detach/list/status/kill (PoC 01-02 を本実装に取り込み)
4. **v0.0.2〜**: send/keys/paste/wait/tail を順次追加

PoC で得た知見は各 finding ファイルに分散、本実装で迷ったら参照。

## メタ報告

### PoC ワークフロー所感

- **examples/ 配置** ([cargo の規約](https://doc.rust-lang.org/cargo/reference/cargo-targets.html#examples)) は実用的: `cargo run --example NAME` で即動、build/test と連動、workspace に乗る、deps 追加不要
- **`test` role を 1 binary に内包** (PoC 02/05) で「自動テスト + 手動再現」両立。daemon role と test role を同じ executable で切り替え
- **想定外発見の頻度**: 軽微な注意点 7-8 件、設計の根幹を覆す発見ゼロ。設計議論の質が良かった
- **PoC 全体の時間**: 8 PoC を 1 セッションで実装+実行+findings 記録、約 2 時間程度。次の本実装フェーズの精度が大幅に上がる投資

### 残ペルソナ視点 (次の本実装で意識)

PoC スコープ外だが本実装で意識すべき:
- **セキュリティエンジニア**: socket permission, token random, env 流出範囲
- **DevOps**: CI で daemon 起動を伴うテストの安定性 (port/socket 競合、子 process leak 対策)
- **UX**: error message の分かりやすさ ("socket parent must be 0700" の自動 fix 提案など)
- **QA**: race condition (= daemon 起動 vs client connect、leader 切替中の resize 等)

これらは v0.0.1 実装の中で TaskCreate で個別タスク化する。
