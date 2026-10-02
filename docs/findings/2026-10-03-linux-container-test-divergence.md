# ローカル Linux コンテナでだけ落ちるテストの真因 (2026-10-03)

対象: v0.9.63 (edd86a42)。issue `docs/issue/2026-10-02-ci-hang-detection-and-local-linux-divergence.md` の (2) serve_tail 系と (3) web_service_e2e の調査記録。

## 凡例

- **根拠種別**: `observed(実機)` = 計装 (一時的な `eprintln!`) やコマンド出力で直接確認、`observed` = コードを読んで確認、`inferred` = 推測 (未検証)
- 環境 L = docker `rust:1.98` (OrbStack、kernel 7.0.14-orbstack、aarch64、debian、`/bin/sh` = dash、root、umask 022)、環境 M = macOS 25.5 (arm64、10 core)、環境 U = GitHub `ubuntu-latest` (ubuntu-24.04 image 20260927.320.1、nextest)
- 再現コマンド (L): `docker run --rm -v "$PWD":/src -w /src -e CARGO_TARGET_DIR=/src/target-linux -v hyoui-cargo-registry:/usr/local/cargo/registry rust:1.98 sh -c 'umask 022; cargo test --locked -p hyoui --lib -- daemon::session::tests::serve_tail' < /dev/null`

## 結論

(2) は互いに独立した 2 つの race で、どちらも「子の出力・exit と client attach の前後関係」が環境で入れ替わることで表に出る。(3) は systemctl 不在時の実装の不整合。いずれも flaky ではなく、観測で決定的な分岐点まで特定した。

| 症状 | 真因 | 責務 | 根拠 |
|---|---|---|---|
| `serve_tail_request_no_follow_dumps_buffer` が 30 秒で `read_until_contains: timed out` | 子が attach 前に `hello` を出すと、`hello` は attach 復元 redraw にだけ載り live raw_data には来ない。test helper (`do_client_handshake`) が redraw を読み捨てるので、test は二度と来ない `hello` を待つ。子 (`sleep 30`) が死んで session.exit.notify が来るまで `Frame::decode_from` が block し、そこで初めて 5 秒の deadline 判定が走る | test | observed(実機) |
| 同じ helper を使う他 5 test (`serve_attach_redraw_includes_pre_attach_output` / `serve_attach_redraw_preserves_alt_screen_flag` / `serve_screen_dump_ansi_returns_state_formatted` / `serve_screen_dump_cbor_returns_encoded_snapshot` / `serve_screen_dump_scrollback_text_plain_returns_old_marker`) | 同上 (issue に挙がっていなかったが同じ race。L で 3 回中 3 回、いずれかが fail) | test | observed(実機) |
| `serve_tail_follow_receives_tail_end_when_child_exits_immediately` が `UnexpectedEof("size header")` | serve_loop で子 exit を検出する 5 点のうち、drain 窓 (= `deferred_exit`、100ms) を通るのは master EOF / EIO の 2 点だけで、SIGCHLD self-pipe / EINTR / Timeout の 3 点は即 `return`。L では子 exit 時に master の POLLHUP と SIGCHLD が同じ poll 周回で ready になり、先に評価される SIGCHLD 経路で抜ける。その周回の `process_pending_handshakes` で登録したばかりの client の tail.request は未処理のまま捨てられ、follower 0 件で TailEnd が誰にも送られない | 実装 | observed(実機) |
| `web_service_e2e` の 2 test が `failed to run systemctl --user is-active ...: No such file or directory` | `SystemdBackend::status` が `is-active` の spawn 失敗だけを `?` で致命扱いし、直後の `show` 系は同じ失敗を `false` に倒していた。systemctl があって user bus に届かない環境では status は成功する (`loaded:false` / `running:false`) ので、「OS に聞けない」理由の違いで status の可否が分かれていた | 実装 | observed(実機) |

## (2-a) attach redraw と live 出力の前後

### 観測

test helper に一時計装を入れ、handshake 直後の redraw frame と以降の frame を全部出した。

| 環境 | redraw body | redraw 後の frame | 結果 |
|---|---|---|---|
| L (3/3) | `\x1b[?1049l\x1b[?25h\x1b[m\x1b[H\x1b[Jhello\x1b>\x1b[?1l\x1b[?2004l` | leader.notify → 30 秒後に session.exit.notify | fail (30.12s) |
| M (3/3) | 空 (`build_attach_redraw` は pristine 画面で空 bytes を返す、`daemon/screen/redraw.rs:29`) | leader.notify → raw_data `hello` | pass (0.04s) |

L では `hello` が redraw に入っており、test はそれを捨てた後に live の `hello` を待っている。panic 位置は deadline 判定 (`session.rs` の `read_until_contains`) だが、実際に止まっていたのは `Frame::decode_from` の block で、deadline は frame 間でしか評価されない。

副次: helper のコメント「`build_attach_redraw` は primary 空画面でも `\x1b[?1049l` prepend + state_formatted の最小 sequence を必ず返す」は現行実装と食い違っていた (pristine なら空)。frame 自体は空 body で必ず 1 つ届く。

### マトリクス (race の発生率)

修正後のコードに計装を入れ、「marker が既に redraw に入っていた回数」(= 修正前なら fail する条件) を各セル 12 回測った。

| セル | `/bin/sh` | CPU | 負荷 | marker in redraw |
|---|---|---|---|---|
| L 既定 | dash | 10 | なし | 8/12 |
| L `--cpus=1` (CFS quota) | dash | 10 (quota 1) | なし | 8/12 |
| L `--cpuset-cpus=0` | dash | 1 | なし | 12/12 |
| L `--cpuset-cpus=0-1` | dash | 2 | なし | 12/12 |
| L `--cpuset-cpus=0-3` | dash | 4 | なし | 7/12 |
| L `--cpuset-cpus=0-3` + `yes` ×4 | dash | 4 | 飽和 | 12/12 |
| L 既定 + `yes` ×10 | dash | 10 | 飽和 | 10/12 |
| L `/bin/sh` → bash | bash | 10 | なし | 0/12 |
| L `/bin/sh` → bash, `--cpuset-cpus=0-3` | bash | 4 | なし | 0/12 |
| M | bash 3.2 (`/private/var/select/sh` → `/bin/bash`) | 10 | なし | 0/10 |

`/bin/sh` の実体が支配的な軸で、起動の速い dash では子が attach 前に `printf` を終えることが多く、bash では 0 回だった。CPU を絞るか飽和させると (= daemon thread と test thread が子と同じ core を取り合うと) 確率が上がる。

### U で通る理由 (inferred)

U の ubuntu-24.04 も `/bin/sh` = dash だが、`serve_tail_request_no_follow_dumps_buffer` は 0.033s で PASS している (= live の `hello` を受け取れた側)。L との違いとして x86_64 / 別 kernel / nextest の 1 test 1 process / hypervisor の差が候補だが、U に計装を入れて確かめてはいない。L の `--cpuset-cpus=0-3` (= U の vCPU 数) でも 7/12 で race するので、U が通るのは「race が起きない環境」だからではなく、その環境での前後関係が偶々逆に寄っているだけと見るのが妥当。どの環境でも test の前提 (= 子の出力は attach 後に来る) は保証されていない。

## (2-b) 子 exit 検出点と drain 窓

### 観測

serve_loop の子 exit 検出点 5 つに一時計装を入れ、どれが最初に exit を見たかと、その時の revents を出した。

| 環境 | 最初の検出点 | 結果 |
|---|---|---|
| L (10 回) | `master-eio` 5 回 / `sigchld` 5 回 | `master-eio` は全 pass、`sigchld` は全 fail (`UnexpectedEof("size header")`) |
| M (10 回) | `master-eof` 10 回 | 全 pass |

`sigchld` で抜けた回の revents は 4/4 で `master_revents=POLLHUP client_revents=[]` だった。master も同じ周回で ready だったが、serve_loop は SIGCHLD を step 0 (master は step 2) で処理するので SIGCHLD 経路が勝つ。`client_revents=[]` なのは、client がその周回の `process_pending_handshakes` で登録された (= poll_fds 構築より後) ため。

Linux の `do_exit` は `exit_files` (= slave close → master POLLHUP) を `exit_notify` (= SIGCHLD) より先に実行するので、daemon thread が両方の後に起きれば同時 ready になる (kernel の順序は inferred、同時 ready 自体は observed(実機))。M は master EOF が SIGCHLD より先に単独で観測される (10/10)。

### マトリクス (修正前なら fail する条件 = `sigchld` が最初の検出点)

| セル | sigchld first |
|---|---|
| L 既定 | 3/12 |
| L `--cpus=1` | 6/12 |
| L `--cpuset-cpus=0` | 12/12 |
| L `--cpuset-cpus=0-1` | 12/12 |
| L `--cpuset-cpus=0-3` | 2/12 |
| L `--cpuset-cpus=0-3` + 負荷 | 2/12 |
| L 既定 + 負荷 | 5/12 |
| L bash | 1/12 |
| L bash `--cpuset-cpus=0-3` | 2/12 |
| M | 0/10 |

こちらは `/bin/sh` にほぼ依存せず (子は `sleep 0.005`)、CPU 数が効く。1〜2 core では毎回 race する。

## (3) web_service_e2e と systemctl

| 環境 | `which systemctl` | `systemctl --user is-active` | `hyoui web service status` (隔離 HOME) | e2e |
|---|---|---|---|---|
| L (rust image のまま) | 無し | spawn 失敗 (ENOENT) | **rc=1**, `failed to run systemctl ...` | 2 fail |
| L + `apt-get install systemd` (PID 1 は systemd でない) | `/usr/bin/systemctl` (systemd 257) | rc=1 `Failed to connect to user scope bus` | rc=0, `loaded:false running:false` | 4 pass |
| U | 有り (user manager の有無は未確認) | — | — | 4 pass |

test は「未登録の隔離 HOME でも status が答える」ことと `service.loaded` / `service.running` が bool であることだけを要求している (`crates/hyoui-cli/tests/web_service_e2e.rs:37`)。DR-0034 決定 6 も status を未登録時にも答える口としている。systemctl の不在は「bus に届かない」と同じく「OS に聞けない」状態なので、status 全体を失敗させるのは実装側の不整合と判断した。

## 修正

- `crates/hyoui/src/daemon/session.rs` serve_loop: 子 exit の全検出点を `defer_child_exit` に通して drain 窓を開く。drain 窓中に master を poll から外す条件を「drain 窓中」から「master を EOF / EIO まで読み切った後」(`master_drained`) に変えた (SIGCHLD で先に exit を見た時、master には子の最後の出力が残りうる)。reap 済の子に対する EOF / EIO は読み切りの印としてだけ扱う (= waitpid が ECHILD で Alive を返し `ALIVE_RETRY_INTERVAL` の retry に落ちるのを避ける)。drain 窓中の master 出力では `--until` を判定しない (kill 相手が居らず、終了理由は子自身の exit)
- 同 test helper: `do_client_handshake_keep_redraw` で redraw body を返し、`read_until_contains` はそれを初期値として連結して探す (= attach 以降に client が観測した子の画面)。6 test をこれに切り替えた。古いコメントを pristine = 空 body に直した
- `crates/hyoui-cli/src/web_service.rs` `SystemdBackend::status`: `is-active` も `show` と同じく、systemctl に聞けない時は `false` / `None` に倒す

## 修正後の確認

| 確認 | 結果 |
|---|---|
| L: `serve_tail_follow_receives_tail_end_when_child_exits_immediately` 単体 15 回 | 15/15 pass (修正前 6/12 fail) |
| L: `serve_tail` + `serve_attach_redraw` + `serve_screen_dump` (10 test) 5 回連続 | 5/5 pass |
| L `--cpuset-cpus=0` / `0-1` (修正前は両 race が 12/12) で同 10 test 5 回連続 | 各 5/5 pass |
| L: `web_service_e2e` 5 回連続 (systemctl 無し) | 5/5 pass |
| M: 同 6 marker test 3 回 | 3/3 pass |
| L: `cargo test --locked --workspace` 全件 1 回 | 1480 passed / 0 failed |
| M: `just ci` (fmt + clippy -D warnings + nextest + doc test + release build) | rc=0、nextest 1479 passed |

Linux の clippy は rust image に clippy component が無く未実行 (`web_service.rs` の変更箇所は `cfg(target_os = "linux")` なので M の clippy は通っていない)。U では `just ci` の lint で検査される。
