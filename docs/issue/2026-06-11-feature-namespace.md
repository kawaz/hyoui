# feature: session の namespace 分離 (list の混在防止)

- Date: 2026-06-11
- Status: open (**方式 a で kawaz 合意済み 2026-06-11**。実装待ち)
- 提案者: kawaz (2026-06-11)

## 動機

普段使いの claude (= hyoui 経由で起動してリモート attach / cmux-msg 操作する) と、
特定用途で一斉起動する hyoui+claude 群 (例: idea-storage で過去セッションの要約を
分担させる worker 群) が `hyoui list` で混ざると邪魔。用途グループごとに分離したい。

## 設計案 (推奨: 方式 a = socket dir 分離)

- socket 配置を `$TMPDIR/hyoui-$UID/<namespace>/` に分離する
- namespace の決定: `--namespace=X` flag > env `HYOUI_NAMESPACE` > `default`
  - direnv 運用と相性が良い (= プロジェクトの .envrc に `export HYOUI_NAMESPACE=...` で
    そのプロジェクト内の起動・list・attach が全部自動分離)
- **互換**: 既存の socket dir 直下 (= `$TMPDIR/hyoui-$UID/*.sock`) は `default` namespace
  として扱う (= dir 移動なしで現行セッションがそのまま見える)
- `hyoui list` は現在の namespace のみ表示。`--all-namespaces` で NS 列付き横断表示
- attach / kill / input / status 等の session 名解決も namespace スコープ
  (= `--socket` 直指定は従来通り任意パス)
- namespace 名の validate は session_id と同等の whitelist (path traversal 防止)

### 検討した代替案

- (b) session 名 prefix 規約 (`ns/name`) + list フィルタ: 実装軽量だが全 socket probe 後の
  フィルタで混在コストが残る、規約の強制力も弱い
- (c) daemon がメタデータとして ns を保持し list でフィルタ: 旧 daemon との互換処理が
  必要になる割に (a) に対する利点がない

## 論点 (確定)

1. 方式 (a) で確定 (kawaz: 「a がシンプル。既存実装とそう変わらない。指定なしなら ns=default」)
2. flag 名: `--namespace` のみ (短縮なし、ロングオプション基本の CLI 方針通り)
3. `list --all-namespaces` は NS 列追加で表示
4. ns 跨ぎ参照は `--namespace` 指定で十分 (合成 ID `<ns>/<session>` は必要が出たら再検討)
5. **子 env への `HYOUI_NAMESPACE` 注入で確定** (kawaz 提案 2026-06-11):
   - run 時に解決した namespace を子プロセスの env に **常時注入** (= 指定なし時も
     `HYOUI_NAMESPACE=default` を入れる)。ns 内でネスト起動した hyoui が指定なしで
     同 ns を引き継ぐのが自然 (= idea-storage worker が更に hyoui run するケース)
   - flag 経由と env (direnv) 経由で子への伝播挙動が揃う (= 非対称の解消)
   - 前例: tmux の `TMUX`、screen の `STY` (= ラッパーが管理下を env で示す慣行)。
     「hyoui 配下には必ず HYOUI_NAMESPACE がある」不変条件は自己検出にも使える
   - 透過原則との緊張 (= env 注入は子から観測可能) は DR で「namespace 継承の必然」
     として justify する。ns 内から別 ns で起動するには `--namespace=default` 明示
     (MANUAL に記載)
6. **入れ子 (階層 ns) は当面入れない、ただし拡張余地を確保** (2026-06-11 議論):
   - フラットな一意名 (`task-xx-team`) でグルーピングの動機は満たせる。階層が本当に
     必要なのは「親 ns ごと再帰一括操作」のニーズが出た時だけ (YAGNI)
   - 階層を入れると相対/絶対の解決規則 (= ns 内で `--namespace=X` は `parent/X` か `X` か)、
     env 継承での無際限な深化、validate の path traversal 境界、再帰操作の意味論を
     背負うことになり、今の動機に対して過剰
   - **拡張余地**: ns 名 validate で `/` を当面禁止しておく。将来階層が必要になったら
     `/` を区切りとして DR を立てて導入 (= 既存 ns 名と衝突せず後方互換で階層化できる)

## 関連

- [[DR-0006]] CLI ground rules
- [[DR-0004]] subcommand 設計
- 類似思想: cmux-msg の「home に閉じる」(kawaz/claude-cmux-msg DR-0005)
