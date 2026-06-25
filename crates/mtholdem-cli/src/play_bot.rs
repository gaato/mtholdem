//! In-process one-hand bot demo for the CLI.

use std::collections::VecDeque;

use mental_card_core::identity::IdentitySecret;
use mtholdem::betting::{BetAction, Street};
use mtholdem::config::{EconomicSecurity, Roster, SessionConfig};
use mtholdem::{Card, Chips, HandNumber, Seat, SessionId};
use mtholdem_bot::{BotDecisionContext, BotPreset, HeuristicBot};
use mtholdem_peer::{FinishedHand, HostEffect, PeerPhase, PeerSession};
use rand::SeedableRng;
use rand::rngs::{OsRng, StdRng};

const DEFAULT_PLAYERS: usize = 3;
const DEFAULT_SEED: u64 = 7;
const MAX_STEPS: usize = 20_000;

#[derive(Clone, Copy, Debug)]
struct Options {
    players: usize,
    seed: u64,
    preset: BotPreset,
}

#[derive(Debug)]
enum QueuedEffect {
    Public {
        sender_index: usize,
        bytes: Vec<u8>,
    },
    Private {
        recipient: Seat,
        bytes: Vec<u8>,
    },
    Showdown {
        owner: Seat,
        bundle: Box<mtholdem::wire::ShowdownBundle>,
    },
}

struct InProcessHand {
    sessions: Vec<PeerSession>,
    queue: VecDeque<QueuedEffect>,
    finished: Vec<FinishedHand>,
    bot: HeuristicBot,
    bot_rng: StdRng,
    crypto_rng: OsRng,
}

/// Runs one complete in-process hand driven by heuristic bot players.
pub(crate) fn run(args: &[String]) -> Result<(), String> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_play_bot_help();
        return Ok(());
    }
    let options = parse_options(args)?;
    let result = run_hand(options).map_err(|error| error.to_string())?;
    println!(
        "finished hand {} with {} players using {:?}",
        result.hand().to_u64(),
        options.players,
        options.preset
    );
    print!("final stacks:");
    for (index, stack) in result.final_stacks().iter().enumerate() {
        print!(" seat{index}={}", stack.units());
    }
    println!();
    Ok(())
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        players: DEFAULT_PLAYERS,
        seed: DEFAULT_SEED,
        preset: BotPreset::Balanced,
    };
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--players" => {
                index += 1;
                options.players = parse_value(args, index, "--players")?;
            }
            "--seed" => {
                index += 1;
                options.seed = parse_value(args, index, "--seed")?;
            }
            "--bot" => {
                index += 1;
                options.preset = parse_preset(args.get(index).map(String::as_str))?;
            }
            other => return Err(format!("unknown play-bot option: {other}")),
        }
        index += 1;
    }
    if !(2..=10).contains(&options.players) {
        return Err("--players must be in 2..=10".to_owned());
    }
    Ok(options)
}

fn parse_value<T: std::str::FromStr>(
    args: &[String],
    index: usize,
    name: &str,
) -> Result<T, String> {
    args.get(index)
        .ok_or_else(|| format!("{name} requires a value"))?
        .parse()
        .map_err(|_| format!("{name} has an invalid value"))
}

fn parse_preset(value: Option<&str>) -> Result<BotPreset, String> {
    match value.ok_or_else(|| "--bot requires a value".to_owned())? {
        "nit" => Ok(BotPreset::Nit),
        "balanced" => Ok(BotPreset::Balanced),
        "loose-passive" => Ok(BotPreset::LoosePassive),
        "lag" => Ok(BotPreset::Lag),
        "maniac" => Ok(BotPreset::Maniac),
        other => Err(format!("unknown bot preset: {other}")),
    }
}

fn print_play_bot_help() {
    println!("usage: mtholdem play-bot [--players N] [--seed N] [--bot PRESET]");
    println!("presets: nit, balanced, loose-passive, lag, maniac");
}

fn run_hand(options: Options) -> mtholdem::Result<FinishedHand> {
    let mut crypto_rng = OsRng;
    let identities: Vec<_> = (0..options.players)
        .map(|_| IdentitySecret::generate(&mut crypto_rng))
        .collect();
    let roster = Roster::new(identities.iter().map(IdentitySecret::public).collect())?;
    let economics = EconomicSecurity::new(
        roster.len(),
        Chips::from_units(100),
        Chips::from_units(101),
        Chips::from_units(202),
    )?;
    let config = SessionConfig::builder(
        SessionId::random(&mut crypto_rng),
        roster,
        Chips::from_units(1),
        Chips::from_units(2),
        Chips::from_units(100),
    )
    .economics(economics)
    .build()?;
    let sessions = identities
        .iter()
        .enumerate()
        .map(|(index, identity)| {
            PeerSession::new(
                config.clone(),
                IdentitySecret::from_seed(identity.to_seed().expose()),
                Seat::new(index).expect("player index was range-checked"),
                HandNumber::FIRST,
            )
        })
        .collect::<mtholdem::Result<Vec<_>>>()?;

    InProcessHand {
        sessions,
        queue: VecDeque::new(),
        finished: Vec::new(),
        bot: HeuristicBot::from_preset(options.preset),
        bot_rng: StdRng::seed_from_u64(options.seed),
        crypto_rng,
    }
    .run()
}

impl InProcessHand {
    fn run(mut self) -> mtholdem::Result<FinishedHand> {
        for index in 0..self.sessions.len() {
            self.progress_and_handle(index)?;
        }

        let mut steps = 0;
        while let Some(effect) = self.queue.pop_front() {
            steps += 1;
            if steps > MAX_STEPS {
                return Err(mental_card_core::error::protocol(
                    "play-bot exceeded the in-process step limit",
                ));
            }
            match effect {
                QueuedEffect::Public {
                    sender_index,
                    bytes,
                } => {
                    for index in 0..self.sessions.len() {
                        if index == sender_index {
                            continue;
                        }
                        self.sessions[index].apply_public_bytes(&bytes, &mut self.crypto_rng)?;
                        self.progress_and_handle(index)?;
                    }
                }
                QueuedEffect::Private { recipient, bytes } => {
                    let index = recipient.index();
                    self.sessions[index].apply_private_bytes(&bytes)?;
                    self.progress_and_handle(index)?;
                }
                QueuedEffect::Showdown { owner, bundle } => {
                    for index in 0..self.sessions.len() {
                        if index == owner.index() {
                            continue;
                        }
                        self.sessions[index].apply_showdown_bundle((*bundle).clone())?;
                        self.progress_and_handle(index)?;
                    }
                }
            }
        }

        self.finished
            .into_iter()
            .next()
            .ok_or_else(|| mental_card_core::error::protocol("play-bot did not finish the hand"))
    }

    fn progress_and_handle(&mut self, index: usize) -> mtholdem::Result<()> {
        self.sessions[index].progress(&mut self.crypto_rng)?;
        loop {
            let effects = self.sessions[index].drain_effects();
            if effects.is_empty() {
                return Ok(());
            }
            for effect in effects {
                self.handle_effect(index, effect)?;
            }
        }
    }

    fn handle_effect(&mut self, sender_index: usize, effect: HostEffect) -> mtholdem::Result<()> {
        match effect {
            HostEffect::BroadcastPublic(message) => self.queue.push_back(QueuedEffect::Public {
                sender_index,
                bytes: message.to_bytes()?,
            }),
            HostEffect::SendPrivate { recipient, message } => {
                self.queue.push_back(QueuedEffect::Private {
                    recipient,
                    bytes: message.to_bytes(),
                });
            }
            HostEffect::RequestAction {
                actor,
                legal_actions,
            } => {
                let street = betting_street(self.sessions[sender_index].status().phase());
                let context =
                    BotDecisionContext::new(self.sessions.len(), actor, street, legal_actions)
                        .with_hole_cards(self.sessions[sender_index].local_hole_cards())
                        .with_board(board_cards(&self.sessions[sender_index]));
                let action = self.bot.choose_action(&context, &mut self.bot_rng);
                print_action(actor, street, action);
                self.sessions[sender_index].submit_action(action)?;
                self.sessions[sender_index].progress(&mut self.crypto_rng)?;
            }
            HostEffect::PublishShowdownBundle { owner, bundle } => {
                self.queue.push_back(QueuedEffect::Showdown {
                    owner,
                    bundle: Box::new(bundle),
                });
            }
            HostEffect::Finished(result) => self.finished.push(result),
            HostEffect::Persist { .. } | HostEffect::SubmitRecovery(_) => {}
            _ => {
                return Err(mental_card_core::error::protocol(
                    "play-bot does not handle this host effect",
                ));
            }
        }
        Ok(())
    }
}

fn betting_street(phase: PeerPhase) -> Street {
    if let PeerPhase::Betting(street) = phase {
        street
    } else {
        Street::Preflop
    }
}

fn board_cards(session: &PeerSession) -> Vec<Card> {
    session.board().iter().map(|card| card.card()).collect()
}

fn print_action(actor: Seat, street: Street, action: BetAction) {
    match action {
        BetAction::Fold => println!("{street}: {actor} folds"),
        BetAction::Check => println!("{street}: {actor} checks"),
        BetAction::Call => println!("{street}: {actor} calls"),
        BetAction::RaiseTo(amount) => println!("{street}: {actor} raises to {}", amount.units()),
        BetAction::AllIn => println!("{street}: {actor} moves all-in"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_bot_finishes_one_hand_and_conserves_chips() {
        let result = run_hand(Options {
            players: 2,
            seed: 11,
            preset: BotPreset::Balanced,
        })
        .expect("bot hand finishes");

        assert_eq!(
            result
                .final_stacks()
                .iter()
                .map(|stack| stack.units())
                .sum::<u64>(),
            200
        );
    }
}
