# Security warning

`mtholdem-testkit` deliberately uses deterministic, predictable randomness. It is suitable only for
tests and reproducible examples. It must not appear in a production dependency graph or generate
any identity, threshold key, proof, or shuffle used outside a test environment.
