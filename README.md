# mtholdem workspace — Mental Texas Hold'em for Rust

このリポジトリは、信頼できるディーラーを置かない P2P Texas Hold'em を実装するための
**Sans-I/O Rust ワークスペース**です。0.3.0 では、単一 crate だった実装を、再利用可能な
暗号カード層、Texas Hold'em 固有層、テスト支援層へ分離しました。

> **重要:** これは研究・試作用の未監査ソフトウェアです。独立した暗号監査、形式検証、
> 本番環境での侵入試験は実施されていません。実金、賞金、換金可能な資産を扱う前に、
> 暗号実装だけでなく、通信、永続化、乱数、復旧 mediator、決済コントラクトまで含めた
> 第三者レビューが必要です。

## Workspace 構成

| crate | 責務 | 意図的に含めないもの |
|---|---|---|
| [`mental-card-core`](crates/mental-card-core) | 52 枚標準デッキ、identity、roster、しきい値 ElGamal、鍵保有証明、検証可能 shuffle、public/private reveal、1 deal の暗号状態機械 | betting、Hold'em street、wire payload、checkpoint、recovery、settlement |
| [`mtholdem`](crates/mtholdem) | no-limit Texas Hold'em ルール、deal plan、署名付き wire、public transcript、hole-card receipt、showdown、checkpoint、recovery evidence | socket、timer、database、blockchain 実装 |
| [`mtholdem-testkit`](crates/mtholdem-testkit) | deterministic roster、設定、鍵生成、全員 shuffle の統合テスト fixture | 本番用乱数、検証 bypass、fake proof、秘密値の覗き見 API |

`mtholdem` という名前は残しました。既存利用者にとって最も意味のある facade であり、
`mtholdem::crypto` と `mtholdem::identity` は core の re-export、`mtholdem::types` は
互換 facade として維持しています。汎用部分だけを利用する新しいゲーム crate は `mental-card-core` を直接依存
できます。

## どこまで汎用化したか

今回の汎用化単位は、**秘密カードを持つゲームで再利用できる、標準 52 枚デッキの暗号操作**です。
次の部分は現実に再利用価値があり、ゲームルールを知らなくても安全条件を定義できます。

- immutable roster と seat order
- long-lived Ed25519 identity
- hand/deal ごとの `(n, n)` threshold key setup
- rogue-key 対策の鍵保有証明
- roster 順の verifiable sequential shuffle
- exact deck / position / recipient に束縛された reveal share
- public または指定 recipient 向けの card reconstruction

一方、wire event、checkpoint、recovery transcript、合法 action、payout は generic trait に
押し込めていません。これらの妥当性は「ゲーム規則が次に何を許すか」と「mediator が何を検査
できるか」に依存するため、表面的な共通化は安全上重要な意味を `Vec<u8>` や callback の背後へ
隠してしまいます。新しいゲームは core の `SessionBinding` に、そのゲーム固有の immutable
config digest を渡し、上位 crate で型付き state machine と canonical wire を定義します。

また、2018/303 のような **secret state を持たないゲーム**は、Hold'em 用 encrypted-deck
protocol の feature として混ぜるべきではありません。commit-and-open や VRF で公開カードを
直接抽選する別 protocol family なので、将来実装する場合は core の sibling crate とする設計です。

詳細は [`docs/crate-boundaries.md`](docs/crate-boundaries.md) と
[`docs/architecture.md`](docs/architecture.md) を参照してください。

## 論文との対応

設計判断では次の 3 論文を役割ごとに参照しています。

- **Kaleidoscope (2017/899):** poker 固有フロー、private/public opening、compact checkpoint、
  penalty-aware recovery の基礎。
- **Royale (2018/157):** card operation と game-rule program を分離する考え方、UC composition、
  general card-game functionality。
- **21 / Bringing Down the Complexity (2018/303):** secret state のないゲームは、private-card
  protocol と異なる軽量な構成にできるという境界。

論文の security proof がこの Rust 実装へ自動的に移るわけではなく、本実装自体に UC 証明は
ありません。

## 主な機能

- Bayer–Groth 型 shuffle proof を伴う 52-card encrypted deck
- Chaum–Pedersen DLEQ proof を伴う public/private reveal share
- session、config、hand、phase、sender、sequence、previous digest を束縛する canonical wire
- exact signed private-share set への hole-card receipt commitment
- receipt と public bundle commitment の両方を検査する showdown opening
- 全員署名の compact checkpoint と recovery suffix / equivocation evidence
- no-limit Hold'em の blind、short all-in、side pot、odd chip、muck、uncontested pot
- 5/6/7 枚からの deterministic hand evaluation
- `compensation > maximum_hand_loss` と
  `deposit >= compensation * (players - 1)` の設定時検証

## 実装しないもの

ワークスペースは network server でも settlement system でもありません。ホスト側には少なくとも
次が必要です。

- authenticated broadcast と single total order
- private reveal 用の authenticated confidential channel
- retry、timeout、peer discovery、NAT traversal、rate limit、DoS 対策
- anti-rollback 永続化と secret storage / deletion
- timeout と evidence を検査して collateral を分配する mediator / smart contract
- 法令、年齢確認、ライセンス、KYC/AML、税務、責任あるゲーミング施策

`(n, n)` threshold なので、1 人が share を止めれば off-chain 進行は停止します。可用性は暗号だけ
ではなく、外部 mediator が challenge と deadline を記録し、abort 者を特定して penalty を適用
することで成立します。

## 読み始める順番

局所的なファイルしか読まない人や AI でも全体像を復元しやすいよう、各公開 module には通常より
詳しい Rustdoc を置いています。

1. [`docs/crate-boundaries.md`](docs/crate-boundaries.md)
2. [`docs/architecture.md`](docs/architecture.md)
3. [`crates/mental-card-core/src/session.rs`](crates/mental-card-core/src/session.rs)
4. [`crates/mental-card-core/src/protocol.rs`](crates/mental-card-core/src/protocol.rs)
5. [`crates/mtholdem/src/protocol.rs`](crates/mtholdem/src/protocol.rs)
6. [`crates/mtholdem/src/wire.rs`](crates/mtholdem/src/wire.rs) と
   [`crates/mtholdem/src/transcript.rs`](crates/mtholdem/src/transcript.rs)
7. [`crates/mtholdem/src/checkpoint.rs`](crates/mtholdem/src/checkpoint.rs) と
   [`crates/mtholdem/src/recovery.rs`](crates/mtholdem/src/recovery.rs)
8. [`crates/mtholdem/src/holdem.rs`](crates/mtholdem/src/holdem.rs) と
   [`crates/mtholdem/src/betting.rs`](crates/mtholdem/src/betting.rs)

内部の shuffle 実装は
[`crates/mental-card-core/src/crypto/bg_shuffle.rs`](crates/mental-card-core/src/crypto/bg_shuffle.rs)
に隔離しています。application は private implementation を直接呼ばず、`crypto` と
`CardCryptography` / `HandCryptography` の verified types を通してください。

## Quick start

Rust 1.85 以上が必要です。

```bash
cargo test --locked --workspace --all-targets --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo run --locked -p mtholdem --example simulated_hand
```

最小の Hold'em session 設定例です。金額は integer chip unit で、浮動小数点は使用しません。

```rust
use mtholdem::config::{EconomicSecurity, Roster, SessionConfig};
use mtholdem::identity::IdentitySecret;
use mtholdem::{Chips, Result, SessionId};
use rand::rngs::OsRng;

fn make_config() -> Result<SessionConfig> {
    let mut rng = OsRng;
    let identities: Vec<_> = (0..3)
        .map(|_| IdentitySecret::generate(&mut rng))
        .collect();
    let roster = Roster::new(
        identities.iter().map(IdentitySecret::public).collect(),
    )?;
    let economics = EconomicSecurity::new(
        roster.len(),
        Chips::from_units(100),
        Chips::from_units(101),
        Chips::from_units(202),
    )?;

    SessionConfig::builder(
        SessionId::random(&mut rng),
        roster,
        Chips::from_units(1),
        Chips::from_units(2),
        Chips::from_units(100),
    )
    .economics(economics)
    .timeout_millis(30_000)
    .build()
}
```

暗号 shuffle、hole-card 配布、board 公開、betting、showdown まで通すローカル simulation は
[`crates/mtholdem/examples/simulated_hand.rs`](crates/mtholdem/examples/simulated_hand.rs) にあります。

## Testkit

integration test では次のように deterministic table を準備できます。

```rust
use mtholdem::{CardPosition, HandNumber};
use mtholdem_testkit::TestTable;

let mut table = TestTable::standard(3, [7; 32])?;
let hand = table.prepare_hand(HandNumber::FIRST)?;
let shares = table.public_reveal_shares(&hand, CardPosition::new(0)?)?;
let card = hand
    .cryptography()
    .open_public_card(CardPosition::new(0)?, &shares)?;
# Ok::<(), mtholdem::Error>(())
```

`mtholdem-testkit` の seeded RNG は予測可能です。本番 key や shuffle には絶対に使用しないで
ください。testkit は別 crate なので、production dependency graph に意図せず入ることも防げます。

## 0.2 からの移行

通常の `mtholdem` 利用コードは、多くの場合 import を変更せずに動作します。core 型を直接使う
場合は次のどちらかを選べます。

```rust
use mtholdem::crypto::VerifiedDeck;       // compatibility facade
use mental_card_core::crypto::VerifiedDeck; // direct core dependency
```

workspace 化自体、package version 0.3.0、source tree path は breaking change です。wire artifact
version は変更していません。checkpoint は引き続き format version 2、その他の既存 artifact は
version 1 です。

## Security と validation

- threat model と deployment assumptions: [`SECURITY.md`](SECURITY.md)
- wire compatibility: [`docs/wire-format.md`](docs/wire-format.md)
- settlement contract boundary: [`docs/settlement-contract.md`](docs/settlement-contract.md)
- 実行した検証: [`VALIDATION.md`](VALIDATION.md)
- 変更履歴: [`CHANGELOG.md`](CHANGELOG.md)
- third-party code notice: [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md)

ライセンスは MIT または Apache-2.0 のデュアルライセンスです。
