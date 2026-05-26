# 2026-05-26 夜: Phase 10/11 完走 + v0.1.0 release

ハンドオフ (`/tmp/hyoui-handoff-2026-05-26-night.md`) を引き継いだセッションの
作業ログ。順序: Phase 10 → Phase 11a/b/c → CLI status/tail/wait →
attach detach prefix → v0.1.0 release prep。

## 結論

- v0.1.0 を release commit + push まで完了 (= release.yml が tag + GH Release
  を自動生成)
- 208 tests passing (= handoff 時点 179 から +29)、clippy + fmt clean
- Phase 10 (lock + leader + mode change)、Phase 11 (status / tail / wait
  全 predicate / subscription 切替)、CLI 3 サブコマンド、detach prefix が
  揃って MVP scope 完了

## Phase 10: lock + leader + mode change

`crates/hyoui/src/daemon/session.rs` に集中。

- `SessionState { lock_holder, lock_token }` を session 寿命で持ち回り
- `should_assign_leader()` で leader 不在時のみ Rw 新 client に leader 委譲
- `elevate_next_leader()` で leader detach 時の cascade (= 次の Rw に振る)
- `LockAcquire` は queue 未実装で wait=true でも Denied 返却 (v0.2.0 で実装)
- `LockRelease` は token + holder 一致で解放、不一致は code=`lock.not-held`
- raw_data 書き込み: Ro mode + 非 lock holder は silently drop
- Resize: 非 leader は code=`mode.not-leader` 返却
- 32 hex 文字の lock token を `/dev/urandom` + timestamp/pid/counter fallback で生成

## Phase 11a: scrollback + status

- `Session::serve` が `Scrollback::new(config.scrollback_bytes)` を所有
- master read 直後に `scrollback.push(Instant::now(), bytes)` (= tail/wait の
  data source)
- `StatusQuery` handler が { session_id, child_pid (WNOHANG), clients,
  scrollback_bytes, lock_holder } を返却

## Phase 11b: tail

- `ClientHandle.subscription: Subscription`、default `Raw`
- `Subscription::TailFollow { strip_ansi }` で同じ master bytes が
  `TYPE_RAW_DATA` ではなく `tail.data` CBOR frame として配信される
- `broadcast_master_bytes()` を新設、subscription 別に encoding を分岐 +
  strip_ansi の真偽でキャッシュを 2 個分けて再 encode を回避
- `handle_tail_request()`:
  - since_ms / since_strict / last_bytes / strip_ansi を scrollback に適用
  - follow=false → 1 個の TailData + TailEnd(Eof) を送って終了
  - follow=true → subscription を切替、TailEnd は session 終了まで送らない
- since_strict 失敗 → TailEnd(BufferTruncated) 即返却
- `instant_to_epoch_ms()` helper で Instant → Unix epoch ms 近似変換

## Phase 11c: wait

- `regex = { features = ["std", "perf", "unicode-perl"] }` を workspace dep に追加
  (unicode-perl 必須、`\d` 等の compile 不能だと wait.invalid-pattern で即時 reject)
- `PendingWait` struct を serve_loop で `Vec<PendingWait>` として所有
- `handle_wait_request()`: Pattern を `regex::bytes::Regex::new()` で compile
  失敗時 `wait.invalid-pattern` で reject、Idle{ms:0} は即 Matched
- `update_waits_on_master_bytes()`: 各 wait の accumulated に新 bytes を append
  (strip_escapes / newline_convert_lf 適用)、scan + match で `WaitResult(Matched)`
- `compute_wait_poll_timeout()`: pending_waits の最も早い deadline / idle 期限から
  `PollTimeout` を計算 (空なら NONE で無限 block)
- `check_wait_timeouts()`: poll が timeout で起きた時に Idle 経過/絶対 deadline
  経過を判定して `Matched` / `Timeout`
- client drop 時は `pending_waits.retain` で当該 client の wait を消す

## CLI: status / tail / wait

`crates/hyoui/src/cli.rs` + `crates/hyoui-cli/src/main.rs`:

- 3 サブコマンドの `Config` 構造体 + 共通 helper `parse_session_targeted()` を導入
- predicate parser: `text:<str>` / `pattern:<regex>` / `wait[-idle]:<dur>`
- duration parser: `500ms` / `1s` / `2m` / bare ms
- `ClientConnection` に `recv_frame()` / `recv_control(buffer_raw_data)` 追加
  (= 1-shot CLI が response を 1 つ取り出す用)
- exit code:
  - wait: Matched=0 / Timeout=1 / Cancelled=2 / ChildExited=130
    (※ Round1 fix で ChildExited=3 に変更、130 は SIGINT 慣例と衝突するため)
  - tail / status: 通常 0、connect 失敗 1

## detach prefix (Ctrl-A D)

`crates/hyoui/src/client/attach.rs`:

- `ClientConnection::run` の stdin scan に detach prefix state machine を追加
- `process_detach_prefix()` 純関数を unit test 6 件で網羅
- prefix=Ctrl-A、trigger='d'、Ctrl-A+Ctrl-A=literal escape、Ctrl-A+他キー=両捨て
- screen 慣例に従う (= "no matching command" は discard、tmux と同じ)

## v0.1.0 release

`pkf run bump-version --level=minor` を実行 → **bug 検出**:

`Taskfile.pkl` の `perl -i -pe '... if !$done++' Cargo.toml` は 1 行目に
post-increment で `$done=1` が立ってしまい、Cargo.toml の `[workspace]` で
match 不発のまま flag が立つ → `[workspace.package].version` 行で s/// が走らず
`0.0.0` 据え置き。

**修正方針**: `s/.../$v.../ and $done=1 unless $done`
(= s/// が match した時のみ flag セット、初回 match まで条件評価継続)

検証コマンド:
```bash
printf '[a]\nversion = "0.0.0"\n[b]\nversion = "0.0.0"\n' | \
  perl -pe 'BEGIN{$v=shift @ARGV} s/^version\s*=\s*"[^"]*"/version = "$v"/ and $done=1 unless $done' 0.1.0
```
→ 1 つ目だけ 0.1.0 に置換、2 つ目は据え置き

**Cargo.toml 修正は v0.1.0 release commit 内で手動対応**、Taskfile 修正は
follow-up commit に分離。

→ canonical (= kawaz/bump-semver の Taskfile.pkl) を確認したところ、bump-semver
本体には Cargo.toml の workspace.package 同期 workaround **そのものが無い**
(= bump-semver 自身は VERSION 1 つだけ管理)。perl bug は hyoui の Taskfile に
独自に書かれたもの。canonical 側への PR は不要。

## 残作業 (v0.2.0+)

- wait queue (= lock.acquire wait=true の queue 対応)
- tail.data の chunk 境界保持 (= 現状 buffer dump を 1 個の TailData で潰す)
- TailEnd(ChildExited) を child exit 時に tail subscriber へ broadcast
- detach prefix の customize (--detach-prefix env / option)
- ANSI escape strip の chunk 境界跨ぎ正規化 (= 現状 best-effort)
- attach subcommand 専用 --help (detach key 動作を明記)

### v0.1.0 後 (本セッション内) で完了済

- **bounded queue 厳密化 (Phase 12)**: `Arc<AtomicUsize> queued_bytes` で byte
  単位の cap を check-and-add 方式で enforce、overflow で `error` kind=
  `backpressure.disconnect` を送って当該 client を `shutdown(Both)` で drop。
  e2e test (`yes` 子 + 読まない client) で 4096 byte cap 超過時の disconnect 確認

### 当初想定していたが不要と判明

- **bump-semver canonical 側への perl bug 追従 PR**: canonical
  (kawaz/bump-semver) を確認したところ workspace.package 同期 workaround は
  そのものが存在しない (= bump-semver は VERSION 1 つだけ管理)。perl 文字列は
  hyoui の Taskfile に独自に書かれたもの。canonical 側修正は不要

## 使用 protocol cap

handshake で client が要求する cap は MVP_CAPS = `["data", "lock", "tail-v1",
"wait-l0"]`。daemon 側は同じ集合を MVP_CAPS として持ち、intersect で「実 cap」を
握って response に返す。
