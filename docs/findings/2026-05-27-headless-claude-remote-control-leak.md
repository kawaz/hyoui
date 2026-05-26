# hyoui で headless 起動した claude も Remote Control の影響を受ける

- Date: 2026-05-27
- 発見元: 2026-05-27 cmux-msg + hyoui の検証セッションで kawaz が観察 (検証セッション中に kawaz のスマホアプリから直接 child に話しかけて即時実証された)

## 判明した事実

`hyoui run --mode=headless -- claude --session-id <uuid> "<prompt>"` で claude を完全裏方として起動したつもりでも、起動した claude session は **デフォルトで Remote Control が有効化された状態** で立ち上がる。結果:

- kawaz のスマホ Claude アプリ (= Claude モバイル) の session 一覧に当該 child claude が登場する
- スマホからそのまま対話可能 (= 第三の介入経路がアクティブ)

hyoui 自体は子プロセスの起動と PTY proxy しか行っておらず、これは claude code 側の Remote Control デフォルト動作 (user settings 由来) が hyoui 経由でも継承されたため。hyoui の責任ではないが、「headless で裏方の child を産んだはず」という期待と挙動がズレる。

起動時の tail に以下が出ているのが root cause の証拠:

```
/remote-control is active · Continue here, on your phone, or at https://claude.ai/code/session_...
```

## 実用的な示唆

用途による:

| 用途 | この挙動の評価 |
|---|---|
| バックグラウンドで自動化させたい (= 人が触らない裏方) | **副作用**: 意図せぬ remote access 経路、誤って人が触る、リストが child だらけになる |
| いつでもスマホから介入したい (= 産んだ child を出先からも操作したい) | **嬉しい副産物**: `hyoui attach` の代わりにスマホから直接画面を見て対話できる |

裏方用途で remote control を切りたい場合の選択肢 (検証はしていない、設計案):

1. `claude --remote-control` フラグの逆 (= 明示 off) を `--no-remote-control` 的に追加してもらう (claude 側マター)
2. CLAUDE_CONFIG_DIR を専用ディレクトリにして user settings 側で remote control を無効化 (= 環境分離。本リポの自動化用ディレクトリと割り切る)
3. hyoui run に `--child-env CLAUDE_CONFIG_DIR=<dir>` のような env override を入れる (現状でも `env CLAUDE_CONFIG_DIR=<dir> hyoui run ...` で代替できる)

hyoui 側で何かを実装するというより、ドキュメントで「headless で claude を spawn する用途では Remote Control の挙動に注意」と注記しておくのが妥当。

## 観測の補強: hyoui tail で入力源を区別できるか

検証中に「親 CC が child を観測する手段としての `hyoui tail`」を試したところ、cmux-msg send からの入力と Remote Control からの入力は **どちらも `❯ ...` プロンプト後の発言として scrollback に並ぶ**。csa timeline で `cmux-msg read` / `cmux-msg reply` の tool call が記録される側が確実な観測手段:

| 観測手段 | 何が見えるか |
|---|---|
| `hyoui tail --strip-ansi` | TUI 全体 (装飾文字込み)、入力源は表現から推測するしかない |
| `csa timeline <child-sid>` | user 発言 / think / tool call / response が構造化、cursor 読みで増分取得可 |
| cmux-msg subscribe | cmux-msg 経路のメッセージ通知のみ (Remote Control 経由は見えない) |

→ child の観測は csa timeline を主軸、hyoui tail は TUI 状態を見たい時のラストリゾート、というレイヤ分けが筋。

## 検証の詳細

### 観察手順

1. 親 CC (claude) から `hyoui run --mode=headless --size=120x40 -- claude --session-id <uuid> "<init prompt>"` を nohup background で起動
2. 数秒後、`hyoui tail run-<pid> --strip-ansi` で child claude の画面状態を覗くと `/remote-control is active · Continue here, on your phone, or at https://claude.ai/code/session_<id>` の行が含まれる
3. kawaz のスマホ Claude アプリの session リストに当該 child が登場することを kawaz が目視確認
4. kawaz がスマホから child に直接「テストあああ」と話しかけて、child が反応 → 親に「これって想定内?」と request してきた (= kawaz の振り付け経由) ことで、Remote Control 経路が完全に機能していることを実証

### 関連 claude オプション (claude --help 抜粋)

```
--remote-control [name]
    Start an interactive session with Remote Control enabled (optionally named)
--remote-control-session-name-prefix <prefix>
    Prefix for auto-generated Remote Control session names (default: hostname)
```

明示的に `--remote-control` を渡していなくても有効化される = user settings 側でデフォルト on になっている (kawaz 環境固有の可能性あり、未確認)。`--no-remote-control` 相当のオプションは help に見当たらず。

## 関連

- [[../issue/2026-05-27-cmux-msg-experiment-feedback-v020-refresh]] — このセッション全体のフィードバック
- [[../issue/2026-05-26-feature-claude-tui-automation]] — `hyoui input keys` で claude TUI を自動操作する本命 use case
