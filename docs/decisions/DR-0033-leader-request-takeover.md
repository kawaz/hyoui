# DR-0033: `leader.request` — rw client による leader 奪取 (takeover)

- Status: Active (kawaz 承認 2026-07-31、LR-Q1=a / LR-Q2=a / LR-Q3=b)
- Date: 2026-07-30
- Related: DR-0008 (protocol、§2.2 で `leader.request` を v0.2.0+ 予約)、DR-0027 (web gateway、WS 制御チャネル)、DR-0019 §6 (SIGWINCH → Resize 配線)、DR-0025 (daemon reducer 化)、docs/issue/2026-07-30-request-web-floating-info-panel.md (Phase 2 の動機)

## Context

### 現行の leader 意味論

- **取得**: 接続時、leader 不在なら最初の `Mode::Rw` client が leader を取る
  (`should_assign_leader`、`crates/hyoui/src/daemon/lock.rs`)。2 番目以降の rw client は
  leader を取らない。`rw-no-leader` は「leader を取らない」明示宣言 mode
- **移動**: leader client の切断時のみ。cascade で次の rw client に昇格し
  (`elevate_next_leader`)、`leader.notify` を全 client に broadcast する
- **leader 限定操作**: `resize` (DR-0008 §2.3、非 leader は `mode.not-leader` error)。
  client 側は `LeaderNotify` 受信で自分の leader 状態を追従し、昇格時 (false → true) は
  外側端末サイズで初回 `Resize` を自動送信する (`crates/hyoui/src/client/attach.rs`)

つまり現行では **leader は「先客が切断するまで動かせない」**。自発的な移動手段が無い。

### 動機と必然性 (= 介入 self-check の核)

web 情報パネル Phase 2 (issue 2026-07-30-request-web-floating-info-panel) で、browser 側から
「この WS 接続を leader にする」操作をしたい。leader でなければ resize が通らず
(HTTP fallback は 409)、browser grid と PTY サイズの同期が leader 側の端末に固定される。

外側 API での代替は不可能:

- **attach し直しても取れない**: `should_assign_leader` は「leader 不在」時のみ付与する。
  先客 leader が接続を維持している限り、何度 re-attach しても leader は動かない
- **`hyoui detach --target=others` は過剰介入**: 他 client の接続を切断してしまう。
  「leader だけ動かしたい、他 client の閲覧は継続させたい」という要求に対して破壊的
- **lock は別軸**: lock は raw_data 書き込みの排他 (DR-0022) であり、resize 責務者の
  選定 (leader) とは独立した軸。lock で代用できない

したがって「leader を奪取する protocol message」が最小の追加手段になる。DR-0008 §2.2 が
`leader.request` を v0.2.0+ 予約として確保済みであり、本 DR はその実体化。

## Decision

### 1. `leader.request` message (client → daemon)

```cbor
{
  "kind": "leader.request"
}
```

payload field は無し (= 「自分を leader にせよ」以外の情報が要らない。対象 client の
指定はできない — 自分以外を leader にする操作は導入しない)。

daemon の処理:

1. **mode 検査**: 要求者が `Mode::Ro` なら `error` (code = `mode.not-allowed`) を返して
   終了 (= 観察専用)。**`Mode::Rw` と `Mode::RwNoLeader` はどちらも要求可** (kawaz 裁定
   2026-07-31 LR-Q3=b)。`rw-no-leader` からの要求は「この接続で leader 意思を表明した」
   とみなし、mode を `rw` に遷移させた上で leader を付与する。
   Rationale (kawaz 実体験より): 人間が今触っているデバイス (例: iPad の後発接続) から
   主導権を奪えないのは大問題で、「mode は handshake で確定」という形式的一貫性を
   利用場面より優先してはならない。re-attach 要求は障害でしかない
2. **既に自分が leader**: no-op 成功。要求者 **にのみ** `leader.notify`
   (`client_id = 自分`) を返す。broadcast はしない (= 状態変化が無いのに全 client へ
   通知を流さない。`leader.notify` は冪等なので要求者側の追従処理は既存のまま動く)
3. **奪取**: 現 leader (居れば) の `leader` flag を false に降格 (mode は `rw` のまま、
   接続も維持)、要求者を leader に昇格。全 client に `leader.notify`
   (`client_id = 新 leader`) を broadcast する

新規 message は `leader.request` の 1 個のみ。応答は既存の `leader.notify` (成功) と
`error` (失敗) を再利用し、専用の `leader.ack` は追加しない (= 最小介入。要求者は
broadcast された `leader.notify` の `client_id` が自分と一致することで成功を同期確認
できる。cascade 昇格と takeover 昇格で client 側の受信処理が 1 本化される)。

### 2. 権限モデル: rw なら誰でも奪取可

追加の認可機構は設けない。信頼境界は同 UID (socket perm 0600 + dir 0700、DR-0008 §7)
であり、socket に接続できる時点で `hyoui kill` すら送れる。leader 奪取だけを
それより強い認可で守る意味は無い。

### 3. takeover のみ。譲渡 (release) は導入しない

旧 leader 側から「手放す」操作 (`leader.release` 相当) は入れない:

- 手放したい client は detach すれば cascade で次の rw client に移る (= 既存機構で足りる)
- 「detach せず leader だけ手放す」を許すと、rw client が居るのに leader 不在という
  状態を自発的に作れてしまう。この状態は resize 責務者不在で、次の rw attach まで
  誰も画面サイズを合わせられない中途半端な状態 (cascade 由来の一時的な leader 不在とは
  違い、恒常化しうる)
- 「別の client に leader を渡したい」ユースケースは、受け手側が `leader.request` を
  送れば足りる (= pull 型に統一。push 型の指名譲渡は対象 client 指定・不在時の
  エラー処理など複雑さの割に用途が無い)

### 4. cap flag: `leader-request-v1`

DR-0008 §3.4 の命名規約 (`noun-vN`) に従い `leader-request-v1` を新設、`MVP_CAPS`
(`crates/hyoui/src/protocol/caps.rs`) に追加して daemon / client 双方が advertise する。

- cap 未保持の client から `leader.request` が来たら `error`
  (code = `unsupported-capability`) — 既存の cap gating 流儀のまま
- 新 client → 旧 daemon は cap intersect でこの cap が落ちる。client / gateway は
  「daemon が leader.request 未対応」と判定して明示エラーにする (= `set-v1` の流儀)
- `leader.notify` 自体は既存 `lock` cap の範囲であり変更しない

### 5. 帰結: 昇格側の resize 自動追従 (既存挙動、新規実装なし)

client 実装は `LeaderNotify` 受信で昇格を検知すると初回 `Resize` を自動送信する
(DR-0019 §6 の既存配線、`attach.rs`)。よって takeover 成功の直後:

- **新 leader の端末サイズに PTY が resize される** (CLI attach client の場合)。
  これは仕様であり、takeover の目的そのもの (= resize 責務者の移動)
- 旧 leader は `leader.notify` で降格を検知し、以後 WINCH を受けても `Resize` を
  送らない (既存処理)。daemon 側も非 leader の `resize` を `mode.not-leader` で
  reject する (既存)
- daemon は leader 付け替えそれ自体では resize しない (= サイズをいつ送るかは
  新 leader client の裁量)

### 6. web gateway への配線 (DR-0027 の WS 制御チャネル拡張)

WS text frame の browser ↔ gateway 制御 JSON (`resize` / `attach.info` /
`resize.result` の先例、`crates/hyoui-web/src/ws_attach.rs`) に追加する:

```jsonc
// browser → gateway
{ "kind": "leader.request", "request_id": 1 }

// gateway → browser (resize.result の先例に倣う)
{ "kind": "leader.result", "request_id": 1, "ok": true }
{ "kind": "leader.result", "request_id": 1, "ok": false, "error": "..." }
```

- gateway は当該 WS に対応する daemon 接続から `leader.request` を送り、結果
  (`leader.notify` 受信 or `error`) を `leader.result` で返す
- **状態の反映は既存経路**: gateway は `leader.notify` / `mode.change` 受信時に
  `attach.info` を再 push する配線が既にあるので、情報パネルの leader 表示は
  自動で更新される。`leader.result` は操作の成否 (特に失敗理由) を UI に返すためだけ
- 昇格成功後、gateway は browser の現 grid サイズで resize を実行する (= §5 の
  CLI client の初回 Resize と同じ意味の追従。DR-0027 の「resize 成功後だけ browser
  grid を変更」の規律はそのまま)

### 7. 実装ノート

- daemon handler は `crates/hyoui/src/daemon/control.rs` の既存流儀
  (`handle_*` + `ensure_rw_mode`) で書く。leader flag の付け替えは
  `ClientHandle.leader` の走査 + `leader.notify` broadcast で、`elevate_next_leader`
  と同じ層に置く
- DR-0025 (reducer 化) 進行中だが、leader state はまだ `ClientHandle` 直持ち。
  本機能も現行流儀で実装し、reducer 移行は DR-0025 の該当 Phase に委ねる
- test: unit (mode 別の許可/拒否、既 leader no-op、旧 leader 降格 + broadcast)、
  e2e (2 client attach → 2 番目が takeover → resize 権限の移動を実測)

## 介入 self-check (DR-0014)

- **既存 DR で justify されているか**: DR-0008 §2.2 が `leader.request` を予約済み。
  本 DR がその実体化の判断記録
- **透過原則との関係**: 子 process には一切触れない (= leader は daemon ↔ client 間の
  resize 責務者選定であり、子から見える変化は新 leader が送る `TIOCSWINSZ` のみ。
  それは既存 resize と同じ、justify 済みの介入)
- **最小介入か**: 新 message 1 個 + 新 cap 1 個。応答・通知・resize 追従は全て既存
  機構の再利用。daemon state の追加なし (`ClientHandle.leader` の付け替えのみ)
- **標準機能の再発明ではないか**: leader は hyoui 固有の概念であり、kernel / PTY /
  shell に対応物は無い (tmux の client サイズ調停に近いが、hyoui は multiplexer では
  なく「代表 1 client のサイズに合わせる」既存方針の範囲内)
- **cap flag の必然性**: 新 kind なので旧 daemon は解釈できない。cap negotiation で
  「未対応」を client 側が事前判定できる必要がある (§4)

## Rejected alternatives

### `leader.ack` 専用応答 message

request/ack ペア (`set-v1` / `upgrade-v1` の流儀) も検討したが、成功時の情報は
`leader.notify` broadcast と完全に重複する。ack を足すと client は「ack と notify の
どちらが先に来ても正しく動く」処理を書く必要があり、message を増やした分だけ
順序の組合せが増える。失敗は既存 `error` で足りる。不採用。

### `leader.release` (譲渡・自発返上)

§3 のとおり。detach cascade で実質可能 + 恒常的な leader 不在状態を作れてしまう。
必要が実証されたら別 DR で。

### `hyoui detach --target=others` での代替

他 client の接続ごと切る破壊的操作であり「leader だけ動かす」要求に合わない。
閲覧継続中の client を巻き添えにする。

### 指名譲渡 (`leader.assign <client-id>` push 型)

「あの client を leader にする」は、受け手が `leader.request` を送る pull 型で
表現できる。push 型は対象不在・対象が ro だった場合等のエラー面が増えるだけで
ユースケースが無い。不採用。

### 奪取の拒否権 (旧 leader が拒める)

同 UID 信頼境界 (§2) では拒否権に守る意味が無く、拒否された側の再試行 UX も悪い。
調停が要る運用は人間同士の合意で足りる。不採用。

## Open Questions (kawaz 裁定待ち)

- **LR-Q1: CLI 表面をどうするか**。protocol が入れば `hyoui leader request [session]`
  等の subcommand は後付け可能 (DR-0007 の v0.3.0 「leader CLI」枠)。本 DR の実装
  (裁定済み 2026-07-31 = a: protocol + web 先行、CLI は後続 issue)
  scope を「protocol + web gateway 配線」に限定し CLI は後続 issue とするか、同時に
  出すか。同時に出す場合の命名 (`hyoui leader request` / `hyoui attach --take-leader` /
  その他) も未裁定
- LR-Q2 (裁定済み 2026-07-31 = a: 要求者にのみ notify)。§1 は「要求者にのみ `leader.notify`」としたが、
  「何も返さない (完全 no-op)」「broadcast する (冪等なので害は少ない)」の選択肢もある。
  同期確認可能性 (要求者が完了を検知できる) を優先して個別 notify を推奨
- ~~LR-Q3~~ 裁定済み (2026-07-31 = b): `rw-no-leader` も要求可、要求時点で rw へ遷移して
  leader 付与 (§1 に反映済み)。mode 遷移が入るため、mode を参照する箇所 (status 表示 /
  DR-0029 §5 の resume 判定等) は「遷移後は rw として扱う」ことを実装で確認する

## 関連

- DR-0008 — protocol 設計 (§2.2 kind 予約リスト、§3 cap flags、§7 認証境界)
- DR-0027 — web gateway (WS text 制御 JSON、resize / attach.info の先例)
- DR-0019 §6 — SIGWINCH → Resize 配線 (昇格時の初回 Resize 送信の正本)
- DR-0025 — daemon reducer 化 (leader state の将来の置き場所)
- docs/issue/2026-07-30-request-web-floating-info-panel.md — Phase 2 leader 昇格 (動機)
