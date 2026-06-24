//! Runs one complete three-player hand without a network or settlement backend.
//!
//! This example exercises the public API in the same order a real host application
//! would use it:
//!
//! 1. create long-lived identity keys and an immutable session configuration;
//! 2. generate one ephemeral threshold key share per player;
//! 3. verify every key announcement and every sequential deck shuffle;
//! 4. privately reconstruct all hole cards from signed reveal shares;
//! 5. publicly reconstruct the board;
//! 6. replay a deterministic check-down through the Hold'em rules engine;
//! 7. evaluate the showdown and print chip-conserving payouts.
//!
//! A production application must replace the in-process vectors with authenticated
//! transport, durable transcript storage, deadlines, and an external settlement
//! mediator. In particular, private reveal messages must use confidential channels.

use mtholdem::Result;
use mtholdem::betting::BetAction;
use mtholdem::card::Card;
use mtholdem::config::{EconomicSecurity, Roster, SessionConfig};
use mtholdem::crypto::MentalKeyShare;
use mtholdem::deal::DealPlan;
use mtholdem::holdem::HoldemHand;
use mtholdem::identity::IdentitySecret;
use mtholdem::protocol::HandCryptography;
use mtholdem::types::{CardPosition, Chips, HandNumber, Seat, SessionId};
use rand::rngs::OsRng;

const PLAYER_COUNT: usize = 3;

fn main() -> Result<()> {
    let mut rng = OsRng;
    let identities: Vec<_> = (0..PLAYER_COUNT)
        .map(|_| IdentitySecret::generate(&mut rng))
        .collect();
    let seats: Vec<_> = (0..PLAYER_COUNT)
        .map(Seat::new)
        .collect::<Result<Vec<_>>>()?;

    let roster = Roster::new(identities.iter().map(IdentitySecret::public).collect())?;
    let economics = EconomicSecurity::new(
        PLAYER_COUNT,
        Chips::from_units(100),
        Chips::from_units(101),
        Chips::from_units(202),
    )?;
    let config = SessionConfig::builder(
        SessionId::random(&mut rng),
        roster,
        Chips::from_units(1),
        Chips::from_units(2),
        Chips::from_units(100),
    )
    .economics(economics)
    .build()?;

    // Every peer would own an independent HandCryptography instance and feed it the
    // same signed public messages. One instance is sufficient for this local simulation
    // because all verification paths are still exercised before state is accepted.
    let mut cryptography = HandCryptography::new(config, HandNumber::FIRST);
    let keys: Vec<_> = seats
        .iter()
        .copied()
        .map(|seat| cryptography.generate_key_share(&mut rng, seat))
        .collect::<Result<Vec<_>>>()?;

    for (&seat, key) in seats.iter().zip(&keys) {
        cryptography.register_key(seat, key.public(), key.proof())?;
    }
    cryptography.finalize_key_setup()?;

    for &seat in &seats {
        let contribution = cryptography.create_shuffle(&mut rng, seat)?;
        cryptography.accept_shuffle(seat, contribution)?;
    }

    let button = seats[0];
    let deal = DealPlan::new(PLAYER_COUNT, button, &seats)?;
    let mut hole_cards = Vec::with_capacity(PLAYER_COUNT);
    for &owner in &seats {
        let positions = deal
            .hole_cards(owner)
            .expect("every seat in this example is active")
            .as_array();
        hole_cards.push(open_private_hand(
            &cryptography,
            &mut rng,
            &seats,
            &keys,
            &identities,
            owner,
            positions,
        )?);
    }

    let flop_positions = deal.flop();
    let flop = [
        open_public_card(&cryptography, &mut rng, &seats, &keys, flop_positions[0])?,
        open_public_card(&cryptography, &mut rng, &seats, &keys, flop_positions[1])?,
        open_public_card(&cryptography, &mut rng, &seats, &keys, flop_positions[2])?,
    ];
    let turn = open_public_card(&cryptography, &mut rng, &seats, &keys, deal.turn())?;
    let river = open_public_card(&cryptography, &mut rng, &seats, &keys, deal.river())?;

    let stacks = [Chips::from_units(100); PLAYER_COUNT];
    let mut hand = HoldemHand::new(
        &stacks,
        button,
        Chips::from_units(1),
        Chips::from_units(2),
        Chips::from_units(100),
    )?;

    // Three-handed preflop order with button seat 0 is 0, 1, 2. Seat 0 calls,
    // small blind seat 1 completes, and big blind seat 2 checks.
    hand.apply_action(seats[0], BetAction::Call)?;
    hand.apply_action(seats[1], BetAction::Call)?;
    hand.apply_action(seats[2], BetAction::Check)?;

    hand.open_flop(flop)?;
    check_postflop_street(&mut hand, &seats)?;
    hand.open_turn(turn)?;
    check_postflop_street(&mut hand, &seats)?;
    hand.open_river(river)?;
    check_postflop_street(&mut hand, &seats)?;

    let showdown: Vec<_> = seats
        .iter()
        .copied()
        .zip(hole_cards.iter().copied().map(Some))
        .collect();
    let result = hand.settle_showdown(&showdown)?;

    println!(
        "board: {flop0} {flop1} {flop2} {turn} {river}",
        flop0 = flop[0],
        flop1 = flop[1],
        flop2 = flop[2],
    );
    for ((&seat, hole), &final_stack) in seats.iter().zip(&hole_cards).zip(result.final_stacks()) {
        let payout = result.distribution().payout(seat)?;
        println!(
            "{seat}: {} {}, payout {payout}, final stack {final_stack}",
            hole[0], hole[1]
        );
    }

    Ok(())
}

fn open_private_hand(
    cryptography: &HandCryptography,
    rng: &mut OsRng,
    seats: &[Seat],
    keys: &[MentalKeyShare],
    identities: &[IdentitySecret],
    owner: Seat,
    positions: [CardPosition; 2],
) -> Result<[Card; 2]> {
    let mut reveals = Vec::with_capacity(seats.len() * positions.len());
    for position in positions {
        for ((&sender, key), identity) in seats.iter().zip(keys).zip(identities) {
            reveals.push(cryptography.create_private_reveal(
                rng,
                sender,
                owner,
                position,
                key.secret(),
                identity,
            )?);
        }
    }
    let (cards, _receipt) = cryptography.open_private_hand(owner, positions, &reveals)?;
    Ok(cards)
}

fn open_public_card(
    cryptography: &HandCryptography,
    rng: &mut OsRng,
    seats: &[Seat],
    keys: &[MentalKeyShare],
    position: CardPosition,
) -> Result<Card> {
    let mut shares = Vec::with_capacity(seats.len());
    for (&sender, key) in seats.iter().zip(keys) {
        let (share, proof) =
            cryptography.create_public_reveal_share(rng, sender, position, key.secret())?;
        shares.push((sender, share, proof));
    }
    cryptography.open_public_card(position, &shares)
}

fn check_postflop_street(hand: &mut HoldemHand, seats: &[Seat]) -> Result<()> {
    // Postflop action starts immediately left of button seat 0: seats 1, 2, then 0.
    for &seat in [&seats[1], &seats[2], &seats[0]] {
        hand.apply_action(seat, BetAction::Check)?;
    }
    Ok(())
}
