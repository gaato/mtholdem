# mtholdem-testkit

Deterministic, test-only fixtures for complete `mtholdem` rosters, session configuration,
threshold-key setup, and verified sequential shuffles.

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

The crate uses predictable seeded randomness and must remain a `dev-dependency`. Never use it for
production identities, keys, proof randomness, or shuffles. It provides no validation bypass and
constructs fixtures only through public production APIs.
