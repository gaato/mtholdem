# mtholdem workspace — Mental Texas Hold'em for Rust

このリポジトリは、信頼できるディーラーを置かない P2P Texas Hold'em のための
**Sans-I/O Rust ワークスペース**です。再利用可能な暗号カード操作、Hold'em protocol rules、
peer-session orchestration、byte-oriented adapter boundary、deterministic test fixture を
別 crate に分けています。

> **重要:** これは研究・試作用の未監査ソフトウェアです。独立した暗号監査、形式検証、
> 本番環境での侵入試験は実施されていません。実金、賞金、換金可能な資産を扱う前に、
> 暗号実装だけでなく、通信、永続化、乱数、復旧 mediator、決済コントラクトまで含めた
> 第三者レビューが必要です。

English README: [README.md](README.md)

`main` の API documentation: <https://gaato.github.io/mtholdem/>

## Workspace 構成

| crate | 用途 |
|---|---|
| [`mental-card-core`](crates/mental-card-core) | 標準カード暗号、identity、roster、threshold key、shuffle、reveal |
| [`mtholdem`](crates/mtholdem) | no-limit Texas Hold'em rules、signed wire、transcript、checkpoint、recovery evidence、settlement-facing state |
| [`mtholdem-peer`](crates/mtholdem-peer) | transport-neutral peer-session orchestration |
| [`mtholdem-adapter`](crates/mtholdem-adapter) | CLI/C ABI/downstream WASM package が共有する byte-oriented boundary |
| [`mtholdem-ffi`](crates/mtholdem-ffi) | opaque handle、ABI version、error code、caller-owned buffer を持つ C ABI |
| [`mtholdem-cli`](crates/mtholdem-cli) | scriptable command-line entrypoint |
| [`mtholdem-testkit`](crates/mtholdem-testkit) | downstream integration test 用 deterministic fixture |

`mtholdem` は Hold'em 固有の rules/protocol crate です。汎用カード暗号、identity、roster、
deck state machine だけを利用する crate は `mental-card-core` に直接依存してください。
crate boundary の詳細は [`docs/crate-boundaries.md`](docs/crate-boundaries.md) にあります。

## 参照

実装と境界設計は次を参照しています。

- [Kaleidoscope: An Efficient Poker Protocol with Payment Distribution and
  Penalty Enforcement][paper-kaleidoscope]
- [ROYALE: A Framework for Universally Composable Card Games with Financial
  Rewards and Penalties Enforcement][paper-royale]
- [21 - Bringing Down the Complexity: Fast Composable Protocols for Card Games
  Without Secret State][paper-21]
- [Bayer-Groth 2012, Efficient Zero-Knowledge Argument for Correctness of a
  Shuffle][paper-bg]
- [`ziffle` 0.1.0][ziffle]。派生元の表示は
  [`THIRD_PARTY_NOTICES.md`](crates/mental-card-core/THIRD_PARTY_NOTICES.md) にあります。

論文の security proof がこの Rust 実装へ自動的に移るわけではなく、本実装自体に UC 証明は
ありません。

[paper-kaleidoscope]: https://eprint.iacr.org/2017/899
[paper-royale]: https://eprint.iacr.org/2018/157
[paper-21]: https://eprint.iacr.org/2018/303.pdf
[paper-bg]: http://www0.cs.ucl.ac.uk/staff/J.Groth/MinimalShuffle.pdf
[ziffle]: https://github.com/v26-solutions/ziffle

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

## Quick start

Rust 1.85 以上が必要です。

```bash
cargo test --locked --workspace --all-targets --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo run --locked -p mtholdem --example simulated_hand
cargo run --locked -p mtholdem-cli -- abi-version
```

最小の Hold'em session 設定例です。金額は integer chip unit で、浮動小数点は使用しません。

```rust
use mental_card_core::identity::IdentitySecret;
use mtholdem::config::{EconomicSecurity, Roster, SessionConfig};
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

## Security と validation

- threat model と deployment assumptions: [`SECURITY.md`](SECURITY.md)
- wire formats: [`docs/wire-format.md`](docs/wire-format.md)
- settlement contract boundary: [`docs/settlement-contract.md`](docs/settlement-contract.md)
- 実行した検証: [`VALIDATION.md`](VALIDATION.md)
- third-party code notice: [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md)

ライセンスは MIT または Apache-2.0 のデュアルライセンスです。
