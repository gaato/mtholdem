# Crate boundary decisions for the initial workspace API

この文書は「なぜこの 3 crate なのか」「次のゲームを追加するとき何を共通化すべきか」を記録します。
コードを一部だけ読む実装者や AI が、局所的な型名から誤った抽象化を増やさないための設計契約です。

## 結論

```text
mental-card-core
    ↑
    └── mtholdem
          ↑
          └── mtholdem-peer
                ↑
                └── mtholdem-adapter
                      ↑
                      ├── mtholdem-cli
                      ├── mtholdem-ffi
                      └── mtholdem-wasm

mtholdem-testkit depends on production crates only as a downstream dev/test helper.
```

依存方向は一方向です。core は Hold'em を知らず、`mtholdem` は peer driver や testkit を知りません。
これにより production verification crate が transport loop や deterministic test RNG を誤って取り込む
依存 cycle を避けています。

## `mental-card-core` に置く条件

次の全条件を満たす概念だけを core に置きます。

1. card game の betting / turn / payout rule を知らずに invariant を説明できる。
2. misuse を型または constructor validation で拒否できる。
3. 少なくとも 2 つの plausible game crate で同じ semantic contract のまま再利用できる。
4. domain separation に必要な game-specific 値を opaque digest として受け取れる。

現在該当するのは roster、identity、standard-card identity、threshold crypto、verified deck、verified
reveal share、per-deal key/shuffle/reveal orchestration です。

`SessionBinding` はこの境界の要です。core は `configuration_digest` の中身を解釈しませんが、全 proof
context に束縛します。上位ゲーム crate は、roster 以外の consensus-critical immutable value を
canonical encoding して digest に含める責務を持ちます。

## `mtholdem` に残す条件

次の概念は Hold'em rule program と一緒に検証しなければ意味を持ちません。

- blind / betting action / street transition
- hole card が各 seat に 2 枚であること
- flop/turn/river の position plan
- public event の phase と payload の対応
- side pot、eligibility、muck、odd chip
- checkpoint に必要な ledger state
- mediator が recovery 中に要求する「次の正しい message」

これらを generic associated type や `Box<dyn GameRules>` にすると、canonical serialization、exhaustive
validation、mediator parity を別々に実装しやすくなります。現在は concrete type を保ち、次のゲーム
実装で本当に一致した state model が見つかった時点で小さな共通 crate を抽出します。

## wire/checkpoint/recovery を先に汎用化しない理由

Royale は general functionality を game-rule program `GR` で parameterize します。しかし library API
で「任意の bytecode を受け取れば generic」とするだけでは、UC model の contract verification を
実装したことにはなりません。実装側には少なくとも次が必要です。

- every transition の deterministic verifier
- canonical, versioned, bounded artifact
- game-specific state invariant
- external mediator と off-chain peer の同一判定
- payout / penalty の exact semantics

したがって初回公開 API は card crypto operations だけを reusable substrate とし、game execution artifact は
Hold'em crate に残します。

## public-state games は sibling protocol family

2018/303 が扱う Blackjack / Baccarat のような secret state のないゲームは、カードを暗号化して
private opening する必要がありません。commit-and-open coin tossing、preprocessed commitment、VRF
extension などで公開カードを選べます。

その実装を `mental-card-core` の feature にすると、次の問題が生じます。

- incompatible security assumptions と state machine が 1 API に混在する。
- `VerifiedDeck` が「encrypted shuffled deck」と「未公開 value の multiset」の両方を意味してしまう。
- dependency feature unification により不要な暗号 primitive が入る。
- caller が game の secret-state requirement を誤って選択しやすい。

将来は例えば `mental-public-card` のような sibling crate として実装し、identity / strong IDs だけを
さらに小さい crate へ抽出するかを、その時点の実利用に基づいて判断します。

## 標準 52 枚デッキに限定した理由

card representation を const generic deck size や arbitrary value type にすることは技術的には可能です。
しかし現実には、proof CRS、ciphertext count、canonical card encoding、wire bounds、hand evaluator が
連動します。未使用の generic parameter を公開すると API stability と audit surface を増やします。

初回公開 API では `Card`, `Rank`, `Suit` と 52-card cryptographic deck を core に置きます。別 deck の実装が
現れたとき、まず内部 trait で共通化し、test vectors と benchmark で semantic equivalence を確認して
から public abstraction を検討します。

## testkit を別 crate にした理由

`cfg(test)` の helper は downstream integration test から使えません。一方、production crate の
feature として deterministic key/shuffle helper を公開すると、feature leakage と本番誤用の危険が
あります。別 crate なら dependency graph で用途が明示され、`dev-dependencies` にだけ置けます。

さらに testkit は public API だけを組み合わせます。proof verification bypass や internal secret
inspection hook はありません。そのため testkit が通ることは、downstream application が実際に利用
できる API surface の統合確認にもなります。

## peer session を別 crate にした理由

P2P multiplayer には local identity、key share、message ordering、replay rejection、host effects が
必要です。一方で socket、timer、database、UI は deployment ごとに異なります。`mtholdem-peer` は
`SignedPublicMessage`、`SignedPrivateReveal`、`HandCryptography`、checkpoint/recovery artifact を
直接 orchestration し、transport には typed `HostEffect` を返すだけにします。これにより CLI、FFI、
WASM、Godot wrapper は同じ Rust-owned peer state を包めます。

## target wrapper を peer session の外に置く理由

CLI、WASM、C ABI、Godot の各 integration は必要な ABI、buffer ownership、error reporting、runtime
packaging が異なります。一方で、public/private artifact の妥当性、action legality、recovery、
showdown、final result は `mtholdem-peer` が一度だけ検証すべきです。

`mtholdem-adapter` は typed `HostEffect` を byte-oriented event、primitive action/status、stable error
code へ変換します。`mtholdem-ffi` だけが C ABI のために `unsafe` を許可し、opaque handle と
明示的な buffer free API を公開します。Godot GDExtension は別 repository に置き、この C ABI を
`PackedByteArray` や Godot 側の value に変換します。

## naming

- `mtholdem`: Texas Hold'em rules/protocol crate であることを表すため維持。
- `mtholdem-peer`: transport-neutral peer-session orchestration を表すため product prefix を維持。
- `mtholdem-adapter`: target-neutral boundary adapter であり、protocol facade ではないことを表す。
- `mtholdem-ffi`: C ABI を所有する唯一の unsafe 境界であることを表す。
- `mtholdem-cli` / `mtholdem-wasm`: shared adapter を使う target wrapper であることを表す。
- `mental-card-core`: poker 以外にも使えるが、standard-card secret-state protocol であることを表現。
- `mtholdem-testkit`: Hold'em config と wrapper を含むため product prefix を維持。

将来 `mental-card-core` が arbitrary deck / multiple protocol family を持つ保証はありません。名前の
`core` はこの workspace の dependency core を意味し、全カードゲームの universal framework を宣言
するものではありません。

## release and versioning

workspace crate は同じ workspace version を使います。release order は `mental-card-core`、
`mtholdem`、`mtholdem-peer`、`mtholdem-adapter`、target wrapper、`mtholdem-testkit` です。
downstream package の Cargo verification は、依存する同 version の crate が registry に公開された後に
実行します。

wire version は crate version と独立です。crate split だけでは signed bytes を変えず、signed meaning
が変わる場合のみ artifact version を上げます。
