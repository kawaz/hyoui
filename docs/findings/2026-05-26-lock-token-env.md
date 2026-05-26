# Finding: lock token の環境変数継承

- Date: 2026-05-26
- PoC: `crates/hyoui/examples/06-lock-token-env.rs`
- 関連: [[DR-0006]] §7 (Lock + tx)、§12 (環境変数)

## 判明した事実

1. **Unix の env inheritance は素直**: 親が `Command::new().env("KEY", "val")` で子を起動 → 子は `getenv("KEY")` / Rust の `std::env::var("KEY")` で値を取れる
2. **孫プロセスにも継承**: 子が `Command::new()` で孫を起動 → 何も設定しなければ親 (= 子) の env がそのまま渡る (= execve に envp として渡される、明示で削除しない限り残る)
3. **`env_clear()` で初期化可能**: 子の env を空にして必要な variable だけ追加可能
4. **PoC: 4 ケース全 PASS**: 子で取れる、孫に継承される、env_clear で消える、自プロセス内 `std::env::var` で取れる

## 実用的な示唆

### tx の env 注入 ([[DR-0006]] §7)

```rust
fn tx_command(name: &str, lock_token: &str, cmd: &str, args: &[&str]) {
    Command::new(cmd)
        .args(args)
        .env("HYOUI_LOCK_TOKEN", lock_token)
        .env("HYOUI_NAME", name)
        .env("HYOUI_SOCK", socket_path)
        .spawn()
        .expect("spawn tx child")
}
```

子 process が `hyoui send/keys/paste/wait` を呼ぶと、それらが内部で `std::env::var("HYOUI_LOCK_TOKEN")` で token を取り、daemon に提示。

### hyoui send 側 (token 自動継承)

```rust
fn auto_lock_token() -> Option<String> {
    std::env::var("HYOUI_LOCK_TOKEN").ok()
}

fn send_command(args: &SendArgs) -> Result<()> {
    let token = args.lock_token.clone().or_else(auto_lock_token);
    // ... token を含む protocol message を daemon に送る
}
```

`--lock-token` 引数明示 → そっち優先、なければ env から自動継承。

### 環境変数 vs file vs socket message

| 方式 | 評価 |
|---|---|
| **env (採用)** | ⭕ 素直、子・孫まで自然に継承、shell から直接見える、debug 容易 |
| file (`/tmp/hyoui-token`) | ❌ 複数 tx 並列で衝突、cleanup 面倒、外部から読まれるリスク |
| socket message (= 毎回 daemon に問い合わせて token 取得) | ❌ 余計な round-trip、token を「持つ」という抽象が消える |

env が最も筋。Unix の慣用に乗る。

### Security 注意点

- `HYOUI_LOCK_TOKEN` は **同 UID の他プロセスから `/proc/<pid>/environ` で読める** (Linux)、macOS も同様
- ただし hyoui daemon 自体が同 UID 限定 (socket permission で他 UID 弾く)、token の流出は実質「自分の他 process が見る」だけ
- 流出した token を別 process が daemon に出して操作するのは「自分の操作と区別がつかない」、これは Unix 思想と整合 (= 同 UID は basically 同一信頼領域)

→ token の機密性は「同 UID 内では低、別 UID から完全保護」レベル。これで十分 (= hyoui の脅威モデル: 攻撃者は別 UID、同 UID は信頼領域)。

### Token 生成

PoC では `tok_<pid>_<microsecond>` の単純形式、本実装では:
- cryptographic random (32 bytes, base64 url-safe) で 16-32 文字
- 毎 lock で新 token (= 使い回さない)
- daemon 側 `HashMap<token, ClientId>` で検証

```rust
use rand::RngCore;
let mut bytes = [0u8; 24];
rand::thread_rng().fill_bytes(&mut bytes);
let token = base64::encode_url_safe(&bytes);
```

(本実装で `rand` + `base64` crate を追加)

## hyoui 本実装への反映

### Lock acquire flow

```
1. client が LOCK_REQUEST 送信 (mode wait | fail, timeout 設定)
2. daemon が token 生成、lock_holder = client_id、token を返す
3. client (= tx 親) が token を子の env に注入して spawn
4. 子 (= tx 内コマンド) が hyoui send/keys/paste/wait を呼ぶ
5. それらが env から token 取得 → 自動で --lock-token として daemon に送る
6. daemon が token 検証 (= lock_holder と一致) → 認可 → 実行
7. 子 exit → tx 親が LOCK_RELEASE 送信 (or timeout で自動解放)
```

### Nested lock (refcount)

同じ token が再度 lock 要求 → daemon は refcount++ で no-op success。
これで tx 内から更に tx を起動 (= nested tx) しても token が継承されて動く。

```
HYOUI_LOCK_TOKEN=tok_xxx tx_parent
  └─ HYOUI_LOCK_TOKEN=tok_xxx tx_child (継承、同 token で nested lock = no-op)
       └─ HYOUI_LOCK_TOKEN=tok_xxx hyoui send ... (token 自動使用)
```

## 検証の詳細

```
$ cargo run --example 06-lock-token-env
[parent] generated token: tok_42951_1779771916706715
[parent] child 1 stdout: "env_token=tok_42951_1779771916706715"
[parent] child 2 stdout: "child=tok_42951_1779771916706715\ngrandchild=tok_42951_1779771916706715\n"
[parent] child 3 stdout: "no_env=EMPTY"
[parent] case4 (std::env::var): true
[parent] PASS
```

4 ケース全合格。Unix の env inheritance 仕様に乗るだけなので、本実装でも素直に動く。
