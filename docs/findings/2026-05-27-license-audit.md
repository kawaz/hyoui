# Finding: 依存 license 整合性監査 (法務視点) — MIT 公開と互換、整備不要

- Date: 2026-05-27
- 対象: hyoui workspace (`crates/hyoui` + `crates/hyoui-cli`) の `Cargo.lock` 全 transitive deps
- ペルソナ: 法務視点 (license 互換性 + 配布要件)
- 関連: workspace `Cargo.toml` (`license = "MIT"`)、`LICENSE` (MIT, Copyright 2026 Yoshiaki Kawazu)

## 要約 (結論先出し)

**問題なし。** hyoui を MIT で公開して GPL/AGPL/LGPL/MPL 系の copyleft 汚染リスクは
ゼロ。全依存が以下のいずれかに収まる:

- **MIT** 単体 (6 件、workspace 自身を含む)
- **Apache-2.0 OR MIT** デュアル (19 件、MIT 側を選択可能)
- **Apache-2.0 OR BSD-2-Clause OR MIT** トリプル (2 件、MIT 側を選択可能)
- **MIT OR Unlicense** (2 件、MIT 側を選択可能)
- **Apache-2.0** 単体 (3 件、`ciborium` family — 後述)
- **(Apache-2.0 OR MIT) AND Unicode-3.0** (1 件、`unicode-ident`)

**推奨アクション**:

1. **THIRD-PARTY-NOTICES ファイル整備は不要** (= 現状の `LICENSE` のみで OK)。
   理由: (a) Rust crate は source 配布が原則で各 dep の license は crates.io / GitHub で参照可能、
   (b) `cargo install` / Homebrew Formula での bin 配布も、kawaz の既存 Rust リポ
   (`bump-semver`, `authsock-warden`) で同様に THIRD-PARTY 同梱していない慣例、
   (c) Apache-2.0 / Unicode-3.0 の attribution 要件は Rust エコシステムの慣行
   (crate metadata で license 表示) で充足とみなされている
2. **将来 release artifact (= 静的 bin) を別経路で配布する場合** (例: GitHub Release の
   pre-built binary を生 zip で配る等) は、Apache-2.0 の §4(d) (NOTICE ファイル同梱要件)
   を満たすため `cargo about generate` 等で third-party notice を自動生成して同梱する案を
   検討してよい。**現フェーズでは不要**。

## 全 dep license 一覧 (avoid-build-deps + avoid-dev-deps)

`cargo-license v0.7.0` で取得 (= production build に含まれる deps のみ、build script /
test 用 deps は除外)。workspace 自身の 2 crate を含む計 33 件:

| crate | version | license | repository |
|---|---|---|---|
| unicode-ident | 1.0.24 | (Apache-2.0 OR MIT) AND Unicode-3.0 | https://github.com/dtolnay/unicode-ident |
| ciborium | 0.2.2 | Apache-2.0 | https://github.com/enarx/ciborium |
| ciborium-io | 0.2.2 | Apache-2.0 | https://github.com/enarx/ciborium |
| ciborium-ll | 0.2.2 | Apache-2.0 | https://github.com/enarx/ciborium |
| zerocopy | 0.8.48 | Apache-2.0 OR BSD-2-Clause OR MIT | https://github.com/google/zerocopy |
| zerocopy-derive | 0.8.48 | Apache-2.0 OR BSD-2-Clause OR MIT | https://github.com/google/zerocopy |
| arrayvec | 0.7.6 | Apache-2.0 OR MIT | https://github.com/bluss/arrayvec |
| bitflags | 2.11.1 | Apache-2.0 OR MIT | https://github.com/bitflags/bitflags |
| cfg-if | 1.0.4 | Apache-2.0 OR MIT | https://github.com/rust-lang/cfg-if |
| half | 2.7.1 | Apache-2.0 OR MIT | https://github.com/VoidStarKat/half-rs |
| itoa | 1.0.18 | Apache-2.0 OR MIT | https://github.com/dtolnay/itoa |
| libc | 0.2.186 | Apache-2.0 OR MIT | https://github.com/rust-lang/libc |
| proc-macro2 | 1.0.106 | Apache-2.0 OR MIT | https://github.com/dtolnay/proc-macro2 |
| quote | 1.0.45 | Apache-2.0 OR MIT | https://github.com/dtolnay/quote |
| regex | 1.12.3 | Apache-2.0 OR MIT | https://github.com/rust-lang/regex |
| regex-automata | 0.4.14 | Apache-2.0 OR MIT | https://github.com/rust-lang/regex |
| regex-syntax | 0.8.10 | Apache-2.0 OR MIT | https://github.com/rust-lang/regex |
| serde | 1.0.228 | Apache-2.0 OR MIT | https://github.com/serde-rs/serde |
| serde_core | 1.0.228 | Apache-2.0 OR MIT | https://github.com/serde-rs/serde |
| serde_derive | 1.0.228 | Apache-2.0 OR MIT | https://github.com/serde-rs/serde |
| syn | 2.0.117 | Apache-2.0 OR MIT | https://github.com/dtolnay/syn |
| thiserror | 2.0.18 | Apache-2.0 OR MIT | https://github.com/dtolnay/thiserror |
| thiserror-impl | 2.0.18 | Apache-2.0 OR MIT | https://github.com/dtolnay/thiserror |
| unicode-width | 0.2.2 | Apache-2.0 OR MIT | https://github.com/unicode-rs/unicode-width |
| vte | 0.15.0 | Apache-2.0 OR MIT | https://github.com/alacritty/vte |
| crunchy | 0.2.4 | MIT | https://github.com/eira-fransham/crunchy |
| hyoui | 0.1.15 | MIT | https://github.com/kawaz/hyoui |
| hyoui-cli | 0.1.15 | MIT | https://github.com/kawaz/hyoui |
| memoffset | 0.9.1 | MIT | https://github.com/Gilnaa/memoffset |
| nix | 0.31.3 | MIT | https://github.com/nix-rust/nix |
| vt100 | 0.16.2 | MIT | https://github.com/doy/vt100-rust |
| aho-corasick | 1.1.4 | MIT OR Unlicense | https://github.com/BurntSushi/aho-corasick |
| memchr | 2.8.0 | MIT OR Unlicense | https://github.com/BurntSushi/memchr |

### License 種別ごとのカウント

| license expression | 件数 |
|---|---|
| Apache-2.0 OR MIT | 19 |
| MIT | 6 (うち 2 件は hyoui workspace 自身) |
| Apache-2.0 | 3 |
| Apache-2.0 OR BSD-2-Clause OR MIT | 2 |
| MIT OR Unlicense | 2 |
| (Apache-2.0 OR MIT) AND Unicode-3.0 | 1 |

## 問題なしと判定した dep + 理由

### グループ 1: MIT または MIT を選択可能なデュアル/トリプル (29 件)

`Apache-2.0 OR MIT`、`Apache-2.0 OR BSD-2-Clause OR MIT`、`MIT OR Unlicense`、
`MIT` 単体は全て、利用者が **MIT 側を選択**することで hyoui の MIT 配布と完全に
互換となる。Rust エコシステムの標準的なライセンス構成であり、永続的に問題なし。

該当 crate: `arrayvec`, `bitflags`, `cfg-if`, `half`, `itoa`, `libc`, `proc-macro2`,
`quote`, `regex`, `regex-automata`, `regex-syntax`, `serde`, `serde_core`,
`serde_derive`, `syn`, `thiserror`, `thiserror-impl`, `unicode-width`, `vte`,
`zerocopy`, `zerocopy-derive`, `aho-corasick`, `memchr`, `crunchy`, `memoffset`,
`nix`, `vt100`, `hyoui`, `hyoui-cli`。

### グループ 2: Apache-2.0 単体 (3 件) — 問題なし、要 NOTICE 要件を把握

`ciborium`, `ciborium-io`, `ciborium-ll` (= enarx 製の CBOR 実装、`DR-0013` で
hyoui の CBOR transport の正本として採用) は **Apache-2.0 単体**。

- **MIT との互換性**: Apache-2.0 は MIT より厳しい条件 (NOTICE 条項、特許条項、
  改変告知) を持つが、両者は「Apache-2.0 でカバーされる部分は Apache-2.0 で配布、
  MIT でカバーされる部分は MIT で配布」という形で **共存可能** (= GPL のような
  「強い copyleft」ではなく、derivative work 全体を Apache-2.0 にする義務はない)
- **NOTICE 要件 (Apache-2.0 §4)**: source distribution では README/LICENSE 内で
  attribution があれば充足。binary distribution (= 別経路で bin を配る場合) は
  NOTICE ファイル同梱が必要だが、**`cargo install` 経由は source 配布**なので不要
- **特許条項 (§3)**: hyoui が ciborium に何らかの特許を主張する立場ではないため
  実害なし
- **結論**: 現状のまま MIT で `cargo install hyoui-cli` / Homebrew 配布で問題なし

### グループ 3: `unicode-ident` (1 件) — Unicode-3.0 attribution 要件

`unicode-ident` は `(Apache-2.0 OR MIT) AND Unicode-3.0`。

- Apache-2.0/MIT の選択肢に加えて、**Unicode-3.0** (Unicode Inc. License v3) を
  **同時に**満たす必要 (= AND)。これは Unicode 公式データテーブル (XID_Start /
  XID_Continue 等) に対する Unicode コンソーシアムの attribution 要件
- Unicode-3.0 は **permissive** (OSI 認定、SPDX 登録) で、要求は「Unicode データの
  copyright notice 保持」だけ。GPL のような propagation はない
- **充足方法**: crate に同梱されている LICENSE-UNICODE ファイルがそのまま伝搬する
  (= cargo が dep の source を crates.io から取得する際に LICENSE も含まれる)。
  hyoui 側で追加の attribution 同梱は実務上不要
- **結論**: source 配布 (= 標準の `cargo install`) では問題なし

## 注意が必要な dep

**該当なし。** GPL / AGPL / LGPL / MPL / CC-BY-SA / SSPL / Commons Clause 等の
restrictive / copyleft license を持つ dep はゼロ。

参考: workspace 直接 deps (`Cargo.toml` で明示している 8 件) と transitive deps
(計 33 件) を全て確認した結果、license expression に上記の問題系文字列が
含まれるものは検出されなかった。

## 推奨アクション

| # | アクション | 優先度 | 備考 |
|---|---|---|---|
| 1 | 現状維持: `LICENSE` (MIT) のみで配布継続 | — | 現フェーズは何もしなくてよい |
| 2 | 将来、pre-built binary を生 zip / tarball で別経路配布する場合のみ THIRD-PARTY-NOTICES.md を追加 | low | `cargo about generate --format markdown` 等で自動生成可能 |
| 3 | release workflow / Homebrew Formula で third-party license を付帯する慣例を kawaz リポ全体で揃える検討 (= 横断ルール化候補) | low | 現状 `bump-semver` / `authsock-warden` も同梱していない、揃っているのでアクション不要 |
| 4 | 新規 dep 追加時の license チェックを CI に組み込む (`cargo deny check licenses`) | medium | 将来の予防策、別 issue で追跡可能。Cargo.toml で許可リスト宣言 |

## 参考 URL

- SPDX License List: https://spdx.org/licenses/
- Apache License 2.0 (compatibility with MIT): https://www.apache.org/licenses/LICENSE-2.0
- Unicode License v3: https://www.unicode.org/license.txt
- cargo-license: https://github.com/onur/cargo-license
- cargo-about (将来の THIRD-PARTY 自動生成候補): https://github.com/EmbarkStudios/cargo-about
- cargo-deny (CI 用 license policy 検査): https://github.com/EmbarkStudios/cargo-deny
- Rust API Guidelines (license recommendation): https://rust-lang.github.io/api-guidelines/necessities.html#crate-and-its-dependencies-have-a-permissive-license-c-permissive

## 検証の詳細

### 使用ツール

```bash
cargo install --locked cargo-license  # v0.7.0
cargo-license license --avoid-build-deps --avoid-dev-deps --json
```

`--avoid-build-deps` / `--avoid-dev-deps` フラグで、最終バイナリに含まれる
production deps のみに絞り込み (= `proc-macro2`/`syn`/`quote` は serde_derive 等の
proc-macro 経由で build-time に必要だが、本オプションでもなお列挙される 。
これは proc-macro が build script ではなく normal dep として扱われるため。
配布される `hyoui` バイナリには **コード生成結果**しか含まれず実体は入らないが、
法務的には dep として残す方が安全側)。

### kawaz リポ他社事例の確認

| リポ | 直下ファイル | THIRD-PARTY 同梱 |
|---|---|---|
| kawaz/bump-semver | LICENSE のみ | なし |
| kawaz/authsock-warden | LICENSE のみ | なし |
| kawaz/hyoui (本リポ) | LICENSE のみ | なし (現状) |

kawaz 既存リポと揃っているので、特別な対応は不要。

### Apache-2.0 単体 deps の再評価 (= ciborium family)

`DR-0013` で hyoui の CBOR transport (= `screen snapshot` の serialize / wait core
の deserialize) に採用された `ciborium`。代替候補との比較:

| crate | license | 採用根拠 (DR-0013 参照) |
|---|---|---|
| `ciborium` | Apache-2.0 | enarx の活発な maintainer、no_std 対応、`Value` 型が CBOR semantic を素直に表現 |
| `serde_cbor` (旧 pyfisch) | MIT/Apache-2.0 | unmaintained (last release 2020)、置換非推奨 |
| `minicbor` | MIT | derive macro で構造体を直接 CBOR にできる、ただし `Value` 動的型なしで wait_core の柔軟な deserialize が書きづらい |

**結論**: `ciborium` を採用継続 (= maintainer 活発性 + API の表現力で優位)。
Apache-2.0 単体であることは MIT プロジェクトでの利用に問題ないため license 観点での
置換は不要。
