# cmux-msg + hyoui 連携経路の検証フィードバック (v0.1.0 時点)

- Date: 2026-05-27
- 発見元: kawaz/claude-cmux-msg main で hyoui 0.1.0 を試した検証セッション
- 経緯: 検証フィードバックとして docs/issue/ に置いていたものを、検証結果の確定事実として findings に移管 (2026-05-30、v0.2.x で B1-B8 の一部は実装解消済)

## 判明した事実

claude-cmux-msg の動作確認文脈で hyoui 0.1.0 を一通り叩いた検証から得た事実をまとめる。基本動作 (run --mode=headless / list / status / tail / wait / kill / attach / completion) は期待通り動作。multi-client 並列接続も確認。

特筆: **`hyoui run --mode=headless` で claude を起動 → 初期プロンプトで `cmux-msg subscribe` を Monitor 起動させる** ことで、`hyoui input keys` (= [[2026-05-26-feature-claude-tui-automation]] で計画) 無しでも cmux-msg メッセージング経路で child claude を完全に対話的に操作できることを実証した。並列 2 child + 並列 send/reply まで成立。

## 実用的な示唆 / ベストプラクティス

### cmux-msg + hyoui の使い分け

| 経路 | 親→child の入力 | child→親 の出力 |
|---|---|---|
| `hyoui input keys + tail` | 直接キー送信 (subscribe 不要) | tail で scrollback / wait で同期 |
| cmux-msg send/reply | child の subscribe 経由 | reply で永続メッセージ |
| 併用 | input keys で即時操作 | cmux-msg で構造化通信 |

特に **双方向 (child → 親の自発的 reply)** は cmux-msg 経路に残る価値があるので、`input keys` 実装後も cmux-msg 連携経路は捨てなくて良い。

副次的に **`cmux-msg spawn` (= cmux ペインを使う) が使えない環境 (SSH 越し / CI / Codespaces)** でも `hyoui run --mode=headless -- claude ...` で child を産める = hyoui が cmux 不在環境での spawn 経路として機能する。

## 検証の詳細 (v0.1.0 時点)

### v0.1.0 で残っていた bug (検証時点での観察)

#### B1: `--until PATTERN` が機能していない

```
hyoui run --mode=headless --size=80x24 --until "STOPHERE" -- bash -c \
  'for i in 1 2 3; do echo line $i; sleep 0.2; done; echo STOPHERE; echo SHOULDNOT'
```

期待: `STOPHERE` 出力時点で hyoui が子を terminate
実際: `SHOULDNOT` まで全部出る (exit 0)。`--until` が無視されている。

v0.2.0 では `wait --pattern` family が proper 実装される ([[../decisions/DR-0010]]) ので、`run --until` を `wait --pattern` への redirect として deprecate する判断もあり得る。

#### B2: headless mode で stdin EOF が子に伝わらない

```
echo "1+2+3+4" | hyoui run --mode=headless --size=80x24 -- bc
```

期待: `bc` が `10` を出して EOF で exit
実際: `bc` が read 待ちで hang。host stdin EOF が子 PTY 側にフォワードされていない。

v0.2.0 の `input paste` family が来ても、`run --mode=headless` で stdin パイプから子に流す経路は残るべき (= one-shot 自動化の最短経路)。

#### B5: `--socket` 明示時の dir mode 0700 強制エラー文言

`--socket /tmp/x.sock` で起動すると `precondition violated: socket parent directory must be mode 0700` で蹴られる。security 要件は理解しているので shielding 自体は維持で良いが、初見でハマるのでエラー文言に「TMPDIR / XDG_RUNTIME_DIR 配下を使うか、`--socket` 親ディレクトリを `chmod 700` してください」を併記すると親切。

#### B8: leader プロセス死亡時に socket ファイルが残骸として残る

`kill -9` 等で daemon が強制終了されると、socket ファイル (`/var/folders/.../T/hyoui-501/run-<pid>.sock`) が削除されず `hyoui list` に残り続ける (`status` は ECONNREFUSED)。改善案:

- `hyoui list` 側を「connect 試行して live なものだけ列挙」に変える
- daemon の atexit hook で socket cleanup を確実にする (= SIGKILL では走らないので前者と併用)

### v0.2.0 scope に組み込みたいと提案した改善

#### B3: `--detached` 相当 (long-lived daemon 起動)

現状の `hyoui run ... &` は leader プロセスが死ぬと socket も消えるため、`nohup ... & disown` で代用が必要。`attach` の `--help` の RELATED 節に `hyoui run --detached    daemon を background 起動` と書かれているが `run --help` には未実装。

[[../decisions/DR-0010]] の v0.2.0 scope = 「外側 API でセッション制御」の前提として **long-lived daemon が前提**になるので、`hyoui run --detached` または専用 spawn subcommand を v0.2.0 初出で揃えると、`input` / `detach` / `lock` family の使い勝手が一段上がる。

cmux-msg のような外部 orchestration 側から「child を spawn して放置 → 後で input/wait/detach で制御」が成立するため、検証セッション中の重要 use case。

#### B4: socket 作成 race (`run` 直後の wait/status が ENOENT)

```
hyoui run --mode=headless --size=80x24 -- bash -c 'sleep 5; echo READY' &
hyoui wait "run-$!" text:READY --timeout=10s
# → hyoui: wait: connect 失敗: syscall failed: ENOENT
```

socket 作成前に wait が走って ENOENT。外側で `sleep 0.5` で回避できるが、v0.2.0 で **外部スクリプトから run → 即 input/wait** を書く前提なら race 解消が必要。改善案:

- wait/status/tail 側が「socket 出現まで短時間 retry」する
- run 側が socket 準備完了を `--ready-fd=3` 等で外部に通知する

#### B6: 個別 `--help` の取りこぼし

0.1.0 では `hyoui list --help` / `hyoui kill --help` / `hyoui completion --help` の 3 つが root help にフォールバック。`run/attach/status/tail/wait` は個別 help あり。

v0.2.0 で 11 → 7 統合 + nested family ([[../decisions/DR-0010]] §1) に再編される際、**全 leaf subcommand に個別 --help を揃える** ことを CI / lint で保証すると、同じ抜けが再発しない。特に nested family (`input text` / `input keys` / `input paste`、`lock acquire` / `lock release` / `lock tx`) は階層各レベル + leaf の help が要る = 抜けが起きやすい構造。

### 仕様確認したい点

#### B7: `kill` subcommand の去就 (v0.2.0 で消える?)

[[../decisions/DR-0010]] §1 で確定した v0.2.0 7 subcommand (`input` / `detach` / `status` / `tail` / `wait` / `lock` / `completion`) に `kill` が含まれていない。一方、v0.1.0 検証で見つかった以下の運用は v0.2.0 でも必要:

- 外部から daemon を確実に終了させる手段
- child claude のように **Ctrl-C 2 連打ガード**を持つ TUI app を相手にする場合の段階的シグナル送信 (`--signal=INT --repeat=2` or `--escalate-to=KILL`)
- 強制終了後の socket cleanup (B8 関連)

選択肢:

1. `kill` を v0.2.0 7 個に追加 (= 8 個)
2. `detach --all --kill-on-empty` 的に detach family に統合 (= 「最後の client が抜けたら daemon 終了」設計)
3. `lock` family に統合 (= 「lock を奪った後で session を終了」設計)
4. kill 経路を CLI から削除 (= 外部から `pkill` する前提)、ただし socket cleanup の責任は daemon 側に必須

### cmux-msg + hyoui 連携で見えた使い方

0.1.0 検証で実証された「**`hyoui run --mode=headless` で claude を spawn → init prompt で `cmux-msg subscribe` を Monitor 起動**」経路は、`hyoui input keys` (= [[2026-05-26-feature-claude-tui-automation]]) が来る前の **代替経路** として機能する:

```
親 CC ── cmux-msg send ──→ child の subscribe Monitor
                              ↓
                          child が cmux-msg reply
                              ↓
親 CC ←── 親側 subscribe Monitor が reply 通知 ←─┘
```

## 関連

- [[../decisions/DR-0010]] — v0.2.0 scope re-scope (本 finding が前提とする scope)
- [[../decisions/DR-0005]] — 外側自動操作主軸の思想
- [[2026-05-26-feature-claude-tui-automation]] — `hyoui input keys` の本命 use case (本 finding と直接接続)
- [[../findings/2026-05-27-headless-claude-remote-control-leak]] — hyoui で headless 起動した claude も Remote Control の影響を受ける別 finding
- 検証元: `kawaz/claude-cmux-msg`
