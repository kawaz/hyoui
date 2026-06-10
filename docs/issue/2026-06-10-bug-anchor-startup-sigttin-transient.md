# bug: anchor 起動直後に子が一過性の T+ (SIGTTIN) になる瞬間がある

- Date: 2026-06-10
- Status: open
- Priority: 低 (= 実害なし。即 S+ に落ち着き、daemon も stopped と誤認しない)

## 現象

DR-0017 の session anchor 実装後、vim / python3 の起動直後に ps stat が一過性で `T+` を
示すことがある (= 複数回サンプルで即 `S+` 定常に収束、daemon の child_stopped も false のまま)。

## 推定原因

anchor setup (= 親の `tcsetpgrp(slave, child)` 確定) と child の最初の tty read が競合する
瞬間に、background pgrp からの read として SIGTTIN が配送される。child 側でも fork 直後に
`tcsetpgrp` しているが、親側の `setpgid(child, child)` との順序により極小の窓が残る。

## 対応案

- 親→子の同期 (= pipe 等で「foreground 確定済み」を合図してから exec) を入れれば窓は閉じる
- ただし fork〜exec 間の async-signal-safe 制約内での実装になる (= pipe read は可)
- 実害が出ていない間は対応不要。attach/観測系で誤検知が出たら着手

## 関連

- [[DR-0017]] §柱 1
- docs/findings/2026-06-10-ctrl-z-two-layer-cause-and-session-anchor-poc.md
