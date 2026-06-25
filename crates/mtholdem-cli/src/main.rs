//! Scriptable command-line entrypoint for `mtholdem`.

#![forbid(unsafe_code)]

mod play_bot;

use std::env;
use std::process::ExitCode;

use mental_card_core::identity::IdentitySecret;
use mtholdem::config::{EconomicSecurity, Roster, SessionConfig};
use mtholdem::{Chips, HandNumber, Seat, SessionId};
use mtholdem_adapter::{
    ABI_VERSION_MAJOR, ABI_VERSION_MINOR, ABI_VERSION_PATCH, BoundaryEffectKind, encode_effects,
};
use mtholdem_peer::PeerSession;
use rand::rngs::OsRng;

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("--version" | "version") => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("abi-version") => {
            println!("{ABI_VERSION_MAJOR}.{ABI_VERSION_MINOR}.{ABI_VERSION_PATCH}");
            ExitCode::SUCCESS
        }
        Some("play-bot") => {
            let args = args.collect::<Vec<_>>();
            match play_bot::run(&args) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("error: {error}");
                    ExitCode::from(1)
                }
            }
        }
        Some("smoke-peer") => match smoke_peer() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::from(1)
            }
        },
        Some("help" | "--help" | "-h") | None => {
            print_help();
            ExitCode::SUCCESS
        }
        Some(command) => {
            eprintln!("unknown command: {command}");
            print_help();
            ExitCode::from(2)
        }
    }
}

fn print_help() {
    println!("mtholdem {}", env!("CARGO_PKG_VERSION"));
    println!("usage: mtholdem <command>");
    println!();
    println!("commands:");
    println!("  abi-version   print the host-boundary ABI version");
    println!("  play-bot      run one in-process hand with heuristic bot players");
    println!("  smoke-peer    create an ephemeral peer and print queued boundary effects");
    println!("  version       print the CLI package version");
}

fn smoke_peer() -> mtholdem::Result<()> {
    let mut rng = OsRng;
    let identities: Vec<_> = (0..2).map(|_| IdentitySecret::generate(&mut rng)).collect();
    let roster = Roster::new(identities.iter().map(IdentitySecret::public).collect())?;
    let economics = EconomicSecurity::new(
        roster.len(),
        Chips::from_units(100),
        Chips::from_units(101),
        Chips::from_units(101),
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
    let mut peer = PeerSession::new(
        config,
        IdentitySecret::from_seed(identities[0].to_seed().expose()),
        Seat::new(0)?,
        HandNumber::FIRST,
    )?;

    peer.progress(&mut rng)?;
    for effect in encode_effects(peer.drain_effects())? {
        println!("{}", effect_name(effect.kind()));
    }
    Ok(())
}

const fn effect_name(kind: BoundaryEffectKind) -> &'static str {
    match kind {
        BoundaryEffectKind::BroadcastPublic => "broadcast-public",
        BoundaryEffectKind::SendPrivate => "send-private",
        BoundaryEffectKind::Persist => "persist",
        BoundaryEffectKind::RequestAction => "request-action",
        BoundaryEffectKind::SubmitRecovery => "submit-recovery",
        BoundaryEffectKind::PublishShowdownBundle => "publish-showdown-bundle",
        BoundaryEffectKind::Finished => "finished",
    }
}
