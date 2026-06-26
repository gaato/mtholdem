//! In-process human-vs-bot play mode for the CLI.

use std::collections::VecDeque;
use std::io::{self, BufRead, Write};

use mental_card_core::identity::IdentitySecret;
use mtholdem::betting::{BetAction, LegalActions, Street};
use mtholdem::config::{EconomicSecurity, Roster, SessionConfig};
use mtholdem::deal::DealPlan;
use mtholdem::holdem::{HandProgress, HoldemHand};
use mtholdem::transcript::TranscriptAnchor;
use mtholdem::{Card, Chips, HandNumber, Rank, Seat, SessionId, Suit};
use mtholdem_bot::{
    ActionIntent, BetSizing, BotDecision, BotDecisionContext, BotPreset, HeuristicBot,
};
use mtholdem_peer::{FinishedHand, HostEffect, PeerPhase, PeerSession};
use mtholdem_table::{TableBackend, TableShowdown};
use rand::SeedableRng;
use rand::rngs::{OsRng, StdRng};
use rand::seq::SliceRandom;

const DEFAULT_PLAYERS: usize = 3;
const DEFAULT_SEED: u64 = 7;
const MAX_STEPS: usize = 20_000;

#[derive(Clone, Copy, Debug)]
struct Options {
    players: usize,
    seed: u64,
    preset: BotPreset,
    human: Option<Seat>,
    hands: Option<u64>,
    backend: TableBackend,
    stats: bool,
    watch_bot: Option<Seat>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HumanInput {
    Action(BetAction),
    Quit,
}

#[derive(Debug)]
enum HandOutcome {
    Finished(HandResult),
    Quit,
}

#[derive(Debug)]
struct HandResult {
    finished: FinishedHand,
    showdown: TableShowdown,
}

#[derive(Debug, Default)]
struct SessionOutcome {
    completed_hands: u64,
    quit: bool,
    final_stacks: Vec<Chips>,
}

#[derive(Clone, Debug, Default)]
struct SeatStats {
    hands: u64,
    vpip: u64,
    pfr: u64,
    rfi_opportunities: u64,
    rfi: u64,
    three_bet_opportunities: u64,
    three_bet: u64,
    fold_to_open_opportunities: u64,
    fold_to_open: u64,
    cold_continue_opportunities: u64,
    cold_continue: u64,
    blind_defense_opportunities: u64,
    blind_defense: u64,
    postflop_actions: u64,
    postflop_bet: u64,
    postflop_raise: u64,
    postflop_call: u64,
    postflop_fold: u64,
    postflop_check: u64,
    flop_cbet_opportunities: u64,
    flop_cbet: u64,
    turn_barrel_opportunities: u64,
    turn_barrel: u64,
}

#[derive(Clone, Debug)]
struct HandStats {
    vpip: Vec<bool>,
    pfr: Vec<bool>,
    voluntary_opened: bool,
    open_raiser: Option<Seat>,
    last_preflop_raiser: Option<Seat>,
    flop_cbetter: Option<Seat>,
    dealt_player_count: usize,
}

impl HandStats {
    fn new(players: usize, dealt_player_count: usize) -> Self {
        Self {
            vpip: vec![false; players],
            pfr: vec![false; players],
            voluntary_opened: false,
            open_raiser: None,
            last_preflop_raiser: None,
            flop_cbetter: None,
            dealt_player_count,
        }
    }
}

#[derive(Debug)]
struct SessionStats {
    enabled: bool,
    seats: Vec<SeatStats>,
    by_dealt_player_count: Vec<SeatStats>,
    current: HandStats,
}

impl SessionStats {
    fn new(players: usize, enabled: bool) -> Self {
        Self {
            enabled,
            seats: vec![SeatStats::default(); players],
            by_dealt_player_count: vec![SeatStats::default(); players + 1],
            current: HandStats::new(players, players),
        }
    }

    fn start_hand(&mut self, stacks: &[Chips]) {
        let dealt_player_count = stacks.iter().filter(|stack| !stack.is_zero()).count();
        self.current = HandStats::new(self.seats.len(), dealt_player_count);
        if !self.enabled {
            return;
        }
        for (index, stack) in stacks.iter().enumerate() {
            if !stack.is_zero() {
                self.seats[index].hands += 1;
            }
        }
        if let Some(stats) = self.by_dealt_player_count.get_mut(dealt_player_count) {
            stats.hands += u64::try_from(dealt_player_count).expect("player count fits u64");
        }
    }

    fn record_action(
        &mut self,
        actor: Seat,
        street: Street,
        context: &BotDecisionContext,
        action: BetAction,
    ) {
        if actor.index() >= self.seats.len() {
            return;
        }

        if street == Street::Preflop {
            self.record_preflop_action(actor, context, action);
        } else if self.enabled {
            self.record_postflop_action(actor, street, context, action);
        }
    }

    fn postflop_initiative(&self, actor: Seat, street: Street) -> bool {
        street != Street::Preflop && self.current.last_preflop_raiser == Some(actor)
    }

    fn record_preflop_action(
        &mut self,
        actor: Seat,
        context: &BotDecisionContext,
        action: BetAction,
    ) {
        let actor_index = actor.index();
        let is_call = matches!(action, BetAction::Call);
        let is_raise = matches!(action, BetAction::RaiseTo(_) | BetAction::AllIn)
            && context.legal_actions.minimum_raise_to().is_some();
        let street_raise_count = context.street_raise_count.unwrap_or(0);
        let is_voluntary = is_call || is_raise;
        let unopened_blinds =
            context
                .big_blind
                .zip(context.current_bet)
                .is_some_and(|(big_blind, current_bet)| {
                    current_bet == big_blind && !context.legal_actions.can_check()
                });

        if unopened_blinds && !self.current.voluntary_opened {
            self.update_actor_stats(actor_index, |stats| stats.rfi_opportunities += 1);
            if is_raise {
                self.update_actor_stats(actor_index, |stats| stats.rfi += 1);
            }
        }

        if let Some(open_raiser) = self.current.open_raiser {
            let facing_open = open_raiser != actor
                && context.legal_actions.to_call() > Chips::ZERO
                && street_raise_count == 1;
            if facing_open {
                self.update_actor_stats(actor_index, |stats| stats.fold_to_open_opportunities += 1);
                self.update_actor_stats(actor_index, |stats| stats.three_bet_opportunities += 1);
                if action == BetAction::Fold {
                    self.update_actor_stats(actor_index, |stats| stats.fold_to_open += 1);
                }
                if is_raise {
                    self.update_actor_stats(actor_index, |stats| stats.three_bet += 1);
                }
            }

            let posted_blind = context
                .actor_street_contributed
                .is_some_and(|contributed| contributed > Chips::ZERO);
            if facing_open && !posted_blind {
                self.update_actor_stats(actor_index, |stats| {
                    stats.cold_continue_opportunities += 1;
                });
                if is_call || is_raise {
                    self.update_actor_stats(actor_index, |stats| stats.cold_continue += 1);
                }
            }
            if facing_open && posted_blind {
                self.update_actor_stats(actor_index, |stats| {
                    stats.blind_defense_opportunities += 1;
                });
                if is_call || is_raise {
                    self.update_actor_stats(actor_index, |stats| stats.blind_defense += 1);
                }
            }
        }

        if is_voluntary && !self.current.vpip[actor_index] {
            self.current.vpip[actor_index] = true;
            self.update_actor_stats(actor_index, |stats| stats.vpip += 1);
        }

        if is_raise {
            if !self.current.pfr[actor_index] {
                self.current.pfr[actor_index] = true;
                self.update_actor_stats(actor_index, |stats| stats.pfr += 1);
            }
            self.current.open_raiser.get_or_insert(actor);
            self.current.last_preflop_raiser = Some(actor);
        }

        if is_voluntary {
            self.current.voluntary_opened = true;
        }
    }

    fn record_postflop_action(
        &mut self,
        actor: Seat,
        street: Street,
        context: &BotDecisionContext,
        action: BetAction,
    ) {
        let is_open_bet = matches!(action, BetAction::RaiseTo(_) | BetAction::AllIn)
            && context.current_bet == Some(Chips::ZERO);
        let is_flop_cbet_opportunity = street == Street::Flop
            && self.current.last_preflop_raiser == Some(actor)
            && context.current_bet == Some(Chips::ZERO)
            && context.legal_actions.can_check();
        let is_turn_barrel_opportunity = street == Street::Turn
            && self.current.flop_cbetter == Some(actor)
            && context.current_bet == Some(Chips::ZERO)
            && context.legal_actions.can_check();
        self.update_actor_stats(actor.index(), |stats| {
            stats.postflop_actions += 1;
            match action {
                BetAction::RaiseTo(_) | BetAction::AllIn if is_open_bet => stats.postflop_bet += 1,
                BetAction::RaiseTo(_) | BetAction::AllIn => stats.postflop_raise += 1,
                BetAction::Call => stats.postflop_call += 1,
                BetAction::Fold => stats.postflop_fold += 1,
                BetAction::Check => stats.postflop_check += 1,
            }
            if is_flop_cbet_opportunity {
                stats.flop_cbet_opportunities += 1;
                if is_open_bet {
                    stats.flop_cbet += 1;
                }
            }
            if is_turn_barrel_opportunity {
                stats.turn_barrel_opportunities += 1;
                if is_open_bet {
                    stats.turn_barrel += 1;
                }
            }
        });
        if street == Street::Flop && is_open_bet {
            self.current.flop_cbetter = Some(actor);
        }
    }

    fn update_actor_stats(&mut self, actor_index: usize, update: impl Fn(&mut SeatStats)) {
        if !self.enabled {
            return;
        }
        update(&mut self.seats[actor_index]);
        if let Some(stats) = self
            .by_dealt_player_count
            .get_mut(self.current.dealt_player_count)
        {
            update(stats);
        }
    }

    fn write_summary(&self, output: &mut impl Write) -> mtholdem::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        writeln!(output, "bot action stats:")
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
        writeln!(
            output,
            "seat hands vpip pfr rfi 3bet fold-to-open cold-continue blind-defense"
        )
        .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
        let mut total = SeatStats::default();
        for stats in &self.seats {
            accumulate_stats(&mut total, stats);
        }
        write_stats_row(output, "all", &total)?;
        for (index, stats) in self.seats.iter().enumerate() {
            write_stats_row(output, &format!("seat{index}"), stats)?;
        }
        writeln!(output, "dealt-player action stats:")
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
        writeln!(
            output,
            "players hands vpip pfr rfi 3bet fold-to-open cold-continue blind-defense"
        )
        .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
        for (count, stats) in self.by_dealt_player_count.iter().enumerate() {
            if stats.hands > 0 {
                write_stats_row(output, &format!("p{count}"), stats)?;
            }
        }
        writeln!(output, "postflop action stats:")
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
        writeln!(output, "seat actions bet raise call fold check")
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
        write_postflop_stats_row(output, "all", &total)?;
        for (index, stats) in self.seats.iter().enumerate() {
            write_postflop_stats_row(output, &format!("seat{index}"), stats)?;
        }
        writeln!(output, "postflop by dealt players:")
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
        writeln!(output, "players actions bet raise call fold check")
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
        for (count, stats) in self.by_dealt_player_count.iter().enumerate() {
            if stats.hands > 0 {
                write_postflop_stats_row(output, &format!("p{count}"), stats)?;
            }
        }
        writeln!(output, "flop continuation-bet stats:")
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
        writeln!(output, "seat cbet")
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
        write_cbet_stats_row(output, "all", &total)?;
        for (index, stats) in self.seats.iter().enumerate() {
            write_cbet_stats_row(output, &format!("seat{index}"), stats)?;
        }
        writeln!(output, "flop continuation-bet by dealt players:")
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
        writeln!(output, "players cbet")
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
        for (count, stats) in self.by_dealt_player_count.iter().enumerate() {
            if stats.hands > 0 {
                write_cbet_stats_row(output, &format!("p{count}"), stats)?;
            }
        }
        writeln!(output, "turn barrel stats:")
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
        writeln!(output, "seat barrel")
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
        write_turn_barrel_stats_row(output, "all", &total)?;
        for (index, stats) in self.seats.iter().enumerate() {
            write_turn_barrel_stats_row(output, &format!("seat{index}"), stats)?;
        }
        writeln!(output, "turn barrel by dealt players:")
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
        writeln!(output, "players barrel")
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
        for (count, stats) in self.by_dealt_player_count.iter().enumerate() {
            if stats.hands > 0 {
                write_turn_barrel_stats_row(output, &format!("p{count}"), stats)?;
            }
        }
        Ok(())
    }
}

fn accumulate_stats(total: &mut SeatStats, stats: &SeatStats) {
    total.hands += stats.hands;
    total.vpip += stats.vpip;
    total.pfr += stats.pfr;
    total.rfi_opportunities += stats.rfi_opportunities;
    total.rfi += stats.rfi;
    total.three_bet_opportunities += stats.three_bet_opportunities;
    total.three_bet += stats.three_bet;
    total.fold_to_open_opportunities += stats.fold_to_open_opportunities;
    total.fold_to_open += stats.fold_to_open;
    total.cold_continue_opportunities += stats.cold_continue_opportunities;
    total.cold_continue += stats.cold_continue;
    total.blind_defense_opportunities += stats.blind_defense_opportunities;
    total.blind_defense += stats.blind_defense;
    total.postflop_actions += stats.postflop_actions;
    total.postflop_bet += stats.postflop_bet;
    total.postflop_raise += stats.postflop_raise;
    total.postflop_call += stats.postflop_call;
    total.postflop_fold += stats.postflop_fold;
    total.postflop_check += stats.postflop_check;
    total.flop_cbet_opportunities += stats.flop_cbet_opportunities;
    total.flop_cbet += stats.flop_cbet;
    total.turn_barrel_opportunities += stats.turn_barrel_opportunities;
    total.turn_barrel += stats.turn_barrel;
}

fn write_stats_row(
    output: &mut impl Write,
    label: &str,
    stats: &SeatStats,
) -> mtholdem::Result<()> {
    writeln!(
        output,
        "{label} {} {} {} {} {} {} {} {}",
        stats.hands,
        format_rate(stats.vpip, stats.hands),
        format_rate(stats.pfr, stats.hands),
        format_rate(stats.rfi, stats.rfi_opportunities),
        format_rate(stats.three_bet, stats.three_bet_opportunities),
        format_rate(stats.fold_to_open, stats.fold_to_open_opportunities),
        format_rate(stats.cold_continue, stats.cold_continue_opportunities),
        format_rate(stats.blind_defense, stats.blind_defense_opportunities),
    )
    .map_err(|error| mental_card_core::error::protocol(error.to_string()))
}

fn write_postflop_stats_row(
    output: &mut impl Write,
    label: &str,
    stats: &SeatStats,
) -> mtholdem::Result<()> {
    writeln!(
        output,
        "{label} {} {} {} {} {} {}",
        stats.postflop_actions,
        format_rate(stats.postflop_bet, stats.postflop_actions),
        format_rate(stats.postflop_raise, stats.postflop_actions),
        format_rate(stats.postflop_call, stats.postflop_actions),
        format_rate(stats.postflop_fold, stats.postflop_actions),
        format_rate(stats.postflop_check, stats.postflop_actions),
    )
    .map_err(|error| mental_card_core::error::protocol(error.to_string()))
}

fn write_cbet_stats_row(
    output: &mut impl Write,
    label: &str,
    stats: &SeatStats,
) -> mtholdem::Result<()> {
    writeln!(
        output,
        "{label} {}",
        format_rate(stats.flop_cbet, stats.flop_cbet_opportunities),
    )
    .map_err(|error| mental_card_core::error::protocol(error.to_string()))
}

fn write_turn_barrel_stats_row(
    output: &mut impl Write,
    label: &str,
    stats: &SeatStats,
) -> mtholdem::Result<()> {
    writeln!(
        output,
        "{label} {}",
        format_rate(stats.turn_barrel, stats.turn_barrel_opportunities),
    )
    .map_err(|error| mental_card_core::error::protocol(error.to_string()))
}

fn format_rate(numerator: u64, denominator: u64) -> String {
    if denominator == 0 {
        return "-".to_owned();
    }
    let percent = numerator.saturating_mul(1000) / denominator;
    format!(
        "{}.{:01}%({numerator}/{denominator})",
        percent / 10,
        percent % 10
    )
}

#[derive(Clone, Debug)]
struct TableState {
    stacks: Vec<Chips>,
    button: Seat,
}

impl TableState {
    fn new(players: usize) -> Self {
        Self {
            stacks: vec![Chips::from_units(100); players],
            button: Seat::new(0).expect("seat zero is valid"),
        }
    }

    fn can_continue(&self) -> bool {
        self.active_player_count() >= 2 && self.maximum_stack() >= Chips::from_units(2)
    }

    fn active_player_count(&self) -> usize {
        self.stacks.iter().filter(|stack| !stack.is_zero()).count()
    }

    fn maximum_stack(&self) -> Chips {
        self.stacks
            .iter()
            .copied()
            .max()
            .unwrap_or(Chips::from_units(100))
    }

    fn advance(&mut self, final_stacks: &[Chips]) {
        self.stacks.clear();
        self.stacks.extend_from_slice(final_stacks);
        self.button = next_button(self.button, &self.stacks);
    }
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
    printed_board_len: usize,
    table_seat_count: usize,
    active_player_count: usize,
    bot: HeuristicBot,
    bot_rng: StdRng,
    watch_bot: Option<Seat>,
    log_hands: bool,
    crypto_rng: OsRng,
}

/// Runs one or more in-process hands driven by a human and heuristic bot players.
pub(crate) fn run(args: &[String]) -> Result<(), String> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_play_bot_help();
        return Ok(());
    }
    let options = parse_options(args)?;
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let outcome =
        run_session(options, &mut input, &mut output).map_err(|error| error.to_string())?;
    println!(
        "session ended after {} completed hand{}",
        outcome.completed_hands,
        if outcome.completed_hands == 1 {
            ""
        } else {
            "s"
        }
    );
    if outcome.quit {
        println!("quit requested");
    }
    Ok(())
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        players: DEFAULT_PLAYERS,
        seed: DEFAULT_SEED,
        preset: BotPreset::Balanced,
        human: Some(Seat::new(0).expect("seat zero is valid")),
        hands: None,
        backend: TableBackend::Peer,
        stats: false,
        watch_bot: None,
    };
    let mut human_explicit = false;
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
            "--human" => {
                index += 1;
                human_explicit = true;
                options.human = Some(parse_seat(args.get(index).map(String::as_str))?);
            }
            "--bots-only" => {
                options.human = None;
            }
            "--hands" => {
                index += 1;
                options.hands = Some(parse_hands(args, index)?);
            }
            "--backend" => {
                index += 1;
                options.backend = parse_backend(args.get(index).map(String::as_str))?;
            }
            "--stats" => {
                options.stats = true;
            }
            "--watch-bot" => {
                index += 1;
                options.watch_bot = Some(parse_seat(args.get(index).map(String::as_str))?);
            }
            other => return Err(format!("unknown play-bot option: {other}")),
        }
        index += 1;
    }
    if !(2..=10).contains(&options.players) {
        return Err("--players must be in 2..=10".to_owned());
    }
    if let Some(human) = options.human {
        if human.index() >= options.players {
            return Err("--human seat must be lower than --players".to_owned());
        }
    }
    if let Some(watch_bot) = options.watch_bot {
        if watch_bot.index() >= options.players {
            return Err("--watch-bot seat must be lower than --players".to_owned());
        }
        if human_explicit {
            return Err("--watch-bot cannot be combined with --human".to_owned());
        }
        options.human = None;
    }
    Ok(options)
}

fn parse_backend(value: Option<&str>) -> Result<TableBackend, String> {
    match value.ok_or_else(|| "--backend requires a value".to_owned())? {
        "peer" | "peer-backed" => Ok(TableBackend::Peer),
        "rules-only" | "rules" => Ok(TableBackend::RulesOnly),
        other => Err(format!("unknown play-bot backend: {other}")),
    }
}

fn parse_hands(args: &[String], index: usize) -> Result<u64, String> {
    let hands = parse_value(args, index, "--hands")?;
    if hands == 0 {
        return Err("--hands must be at least 1".to_owned());
    }
    Ok(hands)
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

fn parse_seat(value: Option<&str>) -> Result<Seat, String> {
    let raw = value.ok_or_else(|| "--human requires a value".to_owned())?;
    let index = raw
        .strip_prefix("seat")
        .unwrap_or(raw)
        .parse()
        .map_err(|_| "--human must be a seat index such as 0 or seat0".to_owned())?;
    Seat::new(index).map_err(|error| error.to_string())
}

fn print_play_bot_help() {
    println!(
        "usage: mtholdem play-bot [--players N] [--seed N] [--bot PRESET] [--human SEAT] [--hands N] [--bots-only] [--backend peer|rules-only] [--stats] [--watch-bot SEAT]"
    );
    println!("presets: nit, balanced, loose-passive, lag, maniac");
    println!("human seat defaults to seat0 and runs until quit; use --hands N to bound it");
    println!("--bots-only runs one hand by default; combine with --hands N for more");
    println!("human seats: 0, 1, ... or seat0, seat1, ...");
    println!("human actions: fold, check, call, bet N, raise N, quit");
    println!("backends: peer, rules-only");
    println!("--stats prints preflop rates, postflop action frequencies, and dealt-count splits");
    println!(
        "        in bots-only mode, --stats suppresses per-hand logs unless --watch-bot is set"
    );
    println!("--watch-bot SEAT runs that seat as a bot and prints its decision details");
    println!("board updates are printed as flop, turn, and river when cards open");
}

fn log_hands(options: Options) -> bool {
    !options.stats || options.human.is_some() || options.watch_bot.is_some()
}

fn run_session(
    options: Options,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> mtholdem::Result<SessionOutcome> {
    let hand_limit = options.hands.or(if options.human.is_none() {
        Some(1)
    } else {
        None
    });
    let mut outcome = SessionOutcome::default();
    let mut table = TableState::new(options.players);
    let mut stats = SessionStats::new(options.players, options.stats);
    outcome.final_stacks.clone_from(&table.stacks);
    loop {
        if hand_limit.is_some_and(|limit| outcome.completed_hands >= limit) {
            stats.write_summary(output)?;
            return Ok(outcome);
        }
        if !table.can_continue() {
            stats.write_summary(output)?;
            return Ok(outcome);
        }
        let hand_number = HandNumber::from_u64(outcome.completed_hands);
        let log_hands = log_hands(options);
        if log_hands {
            writeln!(output, "=== hand {} ===", hand_number.to_u64())
                .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
            write_table_state(options.backend, &table, output)?;
        }
        stats.start_hand(&table.stacks);
        match run_hand(options, hand_number, &table, &mut stats, input, output)? {
            HandOutcome::Finished(result) => {
                outcome.completed_hands += 1;
                if log_hands {
                    write_finished_hand_summary(options, &result, output)?;
                }
                table.advance(result.finished.final_stacks());
                outcome.final_stacks.clone_from(&table.stacks);
            }
            HandOutcome::Quit => {
                outcome.quit = true;
                stats.write_summary(output)?;
                return Ok(outcome);
            }
        }
    }
}

fn write_table_state(
    backend: TableBackend,
    table: &TableState,
    output: &mut impl Write,
) -> mtholdem::Result<()> {
    write!(output, "starting stacks:")
        .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    for (index, stack) in table.stacks.iter().enumerate() {
        write!(output, " seat{index}={}", stack.units())
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    }
    writeln!(
        output,
        " button={} backend={}",
        table.button,
        backend.name()
    )
    .map_err(|error| mental_card_core::error::protocol(error.to_string()))
}

fn write_finished_hand_summary(
    options: Options,
    result: &HandResult,
    output: &mut impl Write,
) -> mtholdem::Result<()> {
    writeln!(
        output,
        "finished hand {} with {} players using {:?} bots",
        result.finished.hand().to_u64(),
        options.players,
        options.preset
    )
    .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    write_showdown_summary(&result.showdown, output)?;
    write!(output, "final stacks:")
        .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    for (index, stack) in result.finished.final_stacks().iter().enumerate() {
        write!(output, " seat{index}={}", stack.units())
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    }
    writeln!(output).map_err(|error| mental_card_core::error::protocol(error.to_string()))
}

fn run_hand(
    options: Options,
    hand_number: HandNumber,
    table: &TableState,
    stats: &mut SessionStats,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> mtholdem::Result<HandOutcome> {
    match options.backend {
        TableBackend::Peer => run_peer_hand(options, hand_number, table, stats, input, output),
        TableBackend::RulesOnly => {
            run_rules_only_hand(options, hand_number, table, stats, input, output)
        }
    }
}

fn run_peer_hand(
    options: Options,
    hand_number: HandNumber,
    table: &TableState,
    stats: &mut SessionStats,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> mtholdem::Result<HandOutcome> {
    let mut crypto_rng = OsRng;
    let identities: Vec<_> = (0..options.players)
        .map(|_| IdentitySecret::generate(&mut crypto_rng))
        .collect();
    let roster = Roster::new(identities.iter().map(IdentitySecret::public).collect())?;
    let buy_in = table.maximum_stack();
    let maximum_hand_loss = buy_in;
    let opponents = u64::try_from(roster.len() - 1).expect("roster length is limited to ten seats");
    let compensation = Chips::from_units(maximum_hand_loss.units() + 1);
    let security_deposit = Chips::from_units(compensation.units() * opponents);
    let economics = EconomicSecurity::new(
        roster.len(),
        maximum_hand_loss,
        compensation,
        security_deposit,
    )?;
    let config = SessionConfig::builder(
        SessionId::random(&mut crypto_rng),
        roster,
        Chips::from_units(1),
        Chips::from_units(2),
        buy_in,
    )
    .economics(economics)
    .build()?;
    let active_seats = active_seats_in_deal_order(table.button, &table.stacks);
    let deal_plan = DealPlan::new(options.players, table.button, &active_seats)?;
    let sessions = identities
        .iter()
        .enumerate()
        .map(|(index, identity)| {
            PeerSession::with_starting_stacks_and_deal_plan(
                config.clone(),
                IdentitySecret::from_seed(identity.to_seed().expose()),
                Seat::new(index).expect("player index was range-checked"),
                hand_number,
                table.stacks.clone(),
                deal_plan.clone(),
            )
        })
        .collect::<mtholdem::Result<Vec<_>>>()?;

    InProcessHand {
        sessions,
        queue: VecDeque::new(),
        finished: Vec::new(),
        printed_board_len: 0,
        table_seat_count: options.players,
        active_player_count: table.active_player_count(),
        bot: HeuristicBot::from_preset(options.preset),
        bot_rng: StdRng::seed_from_u64(options.seed),
        watch_bot: options.watch_bot,
        log_hands: log_hands(options),
        crypto_rng,
    }
    .run(options.human, stats, input, output)
}

fn run_rules_only_hand(
    options: Options,
    hand_number: HandNumber,
    table: &TableState,
    stats: &mut SessionStats,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> mtholdem::Result<HandOutcome> {
    let mut rng = StdRng::seed_from_u64(options.seed ^ hand_number.to_u64().wrapping_mul(7919));
    let mut deck = shuffled_deck(&mut rng);
    let mut hole_cards = vec![None; options.players];
    for seat in active_seats_in_deal_order(table.button, &table.stacks) {
        let first = draw_card(&mut deck)?;
        let second = draw_card(&mut deck)?;
        hole_cards[seat.index()] = Some([first, second]);
    }

    let mut hand = HoldemHand::new(
        &table.stacks,
        table.button,
        Chips::from_units(1),
        Chips::from_units(2),
        table.maximum_stack(),
    )?;
    let bot = HeuristicBot::from_preset(options.preset);
    let log_hands = log_hands(options);

    loop {
        match hand.progress() {
            HandProgress::Betting(street) => {
                let actor = hand.next_to_act().ok_or_else(|| {
                    mental_card_core::error::protocol("rules-only hand has no actor")
                })?;
                let legal_actions = hand.legal_actions()?;
                let action_context = hand.action_context()?;
                let context =
                    BotDecisionContext::new(options.players, actor, street, legal_actions)
                        .with_dealt_player_count(table.active_player_count())
                        .with_hole_cards(hole_cards[actor.index()])
                        .with_board(hand.board().to_vec())
                        .with_action_context(action_context)
                        .with_postflop_initiative(stats.postflop_initiative(actor, street));
                let action = if options.human == Some(actor) {
                    read_human_action(&context, input, output)?
                } else {
                    HumanInput::Action(choose_bot_action(
                        bot,
                        &context,
                        &mut rng,
                        options.watch_bot,
                        output,
                    )?)
                };
                let HumanInput::Action(action) = action else {
                    return Ok(HandOutcome::Quit);
                };
                if log_hands {
                    write_action(actor, street, action, legal_actions, output)?;
                }
                stats.record_action(actor, street, &context, action);
                hand.apply_action(actor, action)?;
            }
            HandProgress::AwaitingFlop => {
                let flop = [
                    draw_card(&mut deck)?,
                    draw_card(&mut deck)?,
                    draw_card(&mut deck)?,
                ];
                hand.open_flop(flop)?;
                if log_hands {
                    write_board_line("flop", hand.board(), output)?;
                }
            }
            HandProgress::AwaitingTurn => {
                hand.open_turn(draw_card(&mut deck)?)?;
                if log_hands {
                    write_board_line("turn", hand.board(), output)?;
                }
            }
            HandProgress::AwaitingRiver => {
                hand.open_river(draw_card(&mut deck)?)?;
                if log_hands {
                    write_board_line("river", hand.board(), output)?;
                }
            }
            HandProgress::Showdown => {
                let mut opened = Vec::new();
                let mut showdown_hands = vec![None; options.players];
                for seat in hand.ledger().live_seats() {
                    let cards = hole_cards[seat.index()];
                    opened.push((seat, cards));
                    showdown_hands[seat.index()] = cards;
                }
                let final_stacks = hand.settle_showdown(&opened)?.final_stacks().to_vec();
                return Ok(HandOutcome::Finished(HandResult {
                    finished: FinishedHand::new(hand_number, TranscriptAnchor::EMPTY, final_stacks),
                    showdown: table_showdown(hand.board(), &showdown_hands)?,
                }));
            }
            HandProgress::Uncontested(_) => {
                let final_stacks = hand.settle_uncontested()?.final_stacks().to_vec();
                return Ok(HandOutcome::Finished(HandResult {
                    finished: FinishedHand::new(hand_number, TranscriptAnchor::EMPTY, final_stacks),
                    showdown: TableShowdown::default(),
                }));
            }
            HandProgress::Complete => {
                let final_stacks = hand
                    .result()
                    .ok_or_else(|| mental_card_core::error::protocol("rules-only result missing"))?
                    .final_stacks()
                    .to_vec();
                return Ok(HandOutcome::Finished(HandResult {
                    finished: FinishedHand::new(hand_number, TranscriptAnchor::EMPTY, final_stacks),
                    showdown: TableShowdown::default(),
                }));
            }
            _ => {
                return Err(mental_card_core::error::protocol(
                    "rules-only backend does not support this hand phase",
                ));
            }
        }
    }
}

impl InProcessHand {
    fn run(
        mut self,
        human: Option<Seat>,
        stats: &mut SessionStats,
        input: &mut impl BufRead,
        output: &mut impl Write,
    ) -> mtholdem::Result<HandOutcome> {
        for index in 0..self.sessions.len() {
            if self.progress_and_handle(index, human, stats, input, output)? {
                return Ok(HandOutcome::Quit);
            }
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
                        if self.progress_and_handle(index, human, stats, input, output)? {
                            return Ok(HandOutcome::Quit);
                        }
                    }
                }
                QueuedEffect::Private { recipient, bytes } => {
                    let index = recipient.index();
                    self.sessions[index].apply_private_bytes(&bytes)?;
                    if self.progress_and_handle(index, human, stats, input, output)? {
                        return Ok(HandOutcome::Quit);
                    }
                }
                QueuedEffect::Showdown { owner, bundle } => {
                    for index in 0..self.sessions.len() {
                        if index == owner.index() {
                            continue;
                        }
                        self.sessions[index].apply_showdown_bundle((*bundle).clone())?;
                        if self.progress_and_handle(index, human, stats, input, output)? {
                            return Ok(HandOutcome::Quit);
                        }
                    }
                }
            }
        }

        let finished =
            self.finished.into_iter().next().ok_or_else(|| {
                mental_card_core::error::protocol("play-bot did not finish the hand")
            })?;
        let showdown_hands = collect_showdown_hands(&self.sessions);
        let board = self.sessions.first().map_or_else(Vec::new, board_cards);
        Ok(HandOutcome::Finished(HandResult {
            finished,
            showdown: table_showdown(&board, &showdown_hands)?,
        }))
    }

    fn progress_and_handle(
        &mut self,
        index: usize,
        human: Option<Seat>,
        stats: &mut SessionStats,
        input: &mut impl BufRead,
        output: &mut impl Write,
    ) -> mtholdem::Result<bool> {
        self.sessions[index].progress(&mut self.crypto_rng)?;
        if self.log_hands {
            self.write_board_updates(output)?;
        }
        loop {
            let effects = self.sessions[index].drain_effects();
            if effects.is_empty() {
                return Ok(false);
            }
            for effect in effects {
                if self.handle_effect(index, effect, human, stats, input, output)? {
                    return Ok(true);
                }
            }
        }
    }

    fn handle_effect(
        &mut self,
        sender_index: usize,
        effect: HostEffect,
        human: Option<Seat>,
        stats: &mut SessionStats,
        input: &mut impl BufRead,
        output: &mut impl Write,
    ) -> mtholdem::Result<bool> {
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
                action_context,
            } => {
                let street = betting_street(self.sessions[sender_index].status().phase());
                let context =
                    BotDecisionContext::new(self.table_seat_count, actor, street, legal_actions)
                        .with_dealt_player_count(self.active_player_count)
                        .with_hole_cards(self.sessions[sender_index].local_hole_cards())
                        .with_board(board_cards(&self.sessions[sender_index]))
                        .with_action_context(action_context)
                        .with_postflop_initiative(stats.postflop_initiative(actor, street));
                let action = if human == Some(actor) {
                    read_human_action(&context, input, output)?
                } else {
                    HumanInput::Action(choose_bot_action(
                        self.bot,
                        &context,
                        &mut self.bot_rng,
                        self.watch_bot,
                        output,
                    )?)
                };
                let HumanInput::Action(action) = action else {
                    return Ok(true);
                };
                if self.log_hands {
                    write_action(actor, street, action, legal_actions, output)?;
                }
                stats.record_action(actor, street, &context, action);
                self.sessions[sender_index].submit_action(action)?;
                self.sessions[sender_index].progress(&mut self.crypto_rng)?;
                if self.log_hands {
                    self.write_board_updates(output)?;
                }
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
        Ok(false)
    }

    fn write_board_updates(&mut self, output: &mut impl Write) -> mtholdem::Result<()> {
        let board = board_cards(&self.sessions[0]);
        if board.len() <= self.printed_board_len || !matches!(board.len(), 3..=5) {
            return Ok(());
        }
        self.printed_board_len = board.len();
        let label = match board.len() {
            3 => "flop",
            4 => "turn",
            5 => "river",
            _ => unreachable!("board updates are filtered to complete public streets"),
        };
        write_board_line(label, &board, output)
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

fn write_board_line(label: &str, board: &[Card], output: &mut impl Write) -> mtholdem::Result<()> {
    writeln!(output, "{label}: {}", format_cards(board))
        .map_err(|error| mental_card_core::error::protocol(error.to_string()))
}

fn shuffled_deck(rng: &mut StdRng) -> Vec<Card> {
    let mut deck = Vec::with_capacity(52);
    for suit in [Suit::Clubs, Suit::Diamonds, Suit::Hearts, Suit::Spades] {
        for rank in [
            Rank::Two,
            Rank::Three,
            Rank::Four,
            Rank::Five,
            Rank::Six,
            Rank::Seven,
            Rank::Eight,
            Rank::Nine,
            Rank::Ten,
            Rank::Jack,
            Rank::Queen,
            Rank::King,
            Rank::Ace,
        ] {
            deck.push(Card::new(rank, suit));
        }
    }
    deck.shuffle(rng);
    deck
}

fn draw_card(deck: &mut Vec<Card>) -> mtholdem::Result<Card> {
    deck.pop()
        .ok_or_else(|| mental_card_core::error::protocol("rules-only deck is empty"))
}

fn active_seats_in_deal_order(button: Seat, stacks: &[Chips]) -> Vec<Seat> {
    (1..=stacks.len())
        .map(|offset| (button.index() + offset) % stacks.len())
        .filter(|index| !stacks[*index].is_zero())
        .map(|index| Seat::new(index).expect("stack index fits in a seat"))
        .collect()
}

fn collect_showdown_hands(sessions: &[PeerSession]) -> Vec<Option<[Card; 2]>> {
    let Some(observer) = sessions.first() else {
        return Vec::new();
    };
    (0..sessions.len())
        .map(|index| observer.showdown_hand(Seat::new(index).expect("session count fits seats")))
        .collect()
}

fn table_showdown(
    board: &[Card],
    showdown_hands: &[Option<[Card; 2]>],
) -> mtholdem::Result<TableShowdown> {
    TableShowdown::from_opened(
        board,
        showdown_hands
            .iter()
            .enumerate()
            .filter_map(|(index, cards)| {
                cards.map(|hole| (Seat::new(index).expect("session count fits seats"), hole))
            }),
    )
}

fn write_showdown_summary(
    showdown: &TableShowdown,
    output: &mut impl Write,
) -> mtholdem::Result<()> {
    if showdown.is_empty() {
        writeln!(output, "showdown: none")
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
        return Ok(());
    }
    write!(output, "showdown:")
        .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    for hand in showdown.hands() {
        let [first, second] = hand.hole_cards;
        let seat = hand.seat.index();
        let category = hand.category;
        write!(output, " seat{seat}={first} {second} ({category})")
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    }
    writeln!(output).map_err(|error| mental_card_core::error::protocol(error.to_string()))
}

fn choose_bot_action(
    bot: HeuristicBot,
    context: &BotDecisionContext,
    rng: &mut impl rand::Rng,
    watch_bot: Option<Seat>,
    output: &mut impl Write,
) -> mtholdem::Result<BetAction> {
    let decision = bot.decide(context, rng);
    if watch_bot == Some(context.actor) {
        write_bot_watch(context, &decision, output)?;
    }
    Ok(decision.action)
}

fn write_bot_watch(
    context: &BotDecisionContext,
    decision: &BotDecision,
    output: &mut impl Write,
) -> mtholdem::Result<()> {
    writeln!(output, "bot-watch: {} {}", context.actor, context.street)
        .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    if let Some([first, second]) = context.hole_cards {
        writeln!(output, "hand: {first} {second}")
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    } else {
        writeln!(output, "hand: unknown")
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    }
    writeln!(output, "board: {}", format_cards(&context.board))
        .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    writeln!(output, "pot: {}", format_optional_chips(context.pot))
        .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    writeln!(
        output,
        "to call: {}",
        context.legal_actions.to_call().units()
    )
    .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    writeln!(
        output,
        "current bet: {}",
        format_optional_chips(context.current_bet)
    )
    .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    writeln!(
        output,
        "live opponents: {}",
        format_optional_usize(context.live_opponents)
    )
    .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    writeln!(
        output,
        "matched opponents: {}",
        format_optional_usize(context.matched_bet_opponents)
    )
    .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    writeln!(
        output,
        "assessment: strength={} equity={} draw={} pressure={} score={} category={}",
        format_float(decision.assessment.strength),
        format_optional_float(decision.assessment.equity),
        format_float(decision.assessment.draw_potential),
        format_float(decision.assessment.pressure),
        format_float(decision.assessment.score),
        decision
            .assessment
            .category
            .map_or_else(|| "-".to_owned(), |category| category.to_string())
    )
    .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    writeln!(
        output,
        "decision: intent={} sizing={} action={}",
        format_intent(decision.intent),
        format_sizing(decision.sizing),
        format_action_name(context.street, decision.action, context.legal_actions)
    )
    .map_err(|error| mental_card_core::error::protocol(error.to_string()))
}

fn format_optional_chips(value: Option<Chips>) -> String {
    value.map_or_else(|| "-".to_owned(), |chips| chips.units().to_string())
}

fn format_optional_usize(value: Option<usize>) -> String {
    value.map_or_else(|| "-".to_owned(), |value| value.to_string())
}

fn format_optional_float(value: Option<f32>) -> String {
    value.map_or_else(|| "-".to_owned(), format_float)
}

fn format_float(value: f32) -> String {
    format!("{value:.2}")
}

const fn format_intent(intent: ActionIntent) -> &'static str {
    match intent {
        ActionIntent::Fold => "fold",
        ActionIntent::Check => "check",
        ActionIntent::Call => "call",
        ActionIntent::Bet => "bet",
        ActionIntent::Raise => "raise",
        ActionIntent::AllIn => "all-in",
    }
}

fn format_sizing(sizing: BetSizing) -> String {
    match sizing {
        BetSizing::None => "none".to_owned(),
        BetSizing::PreflopOpenOrIsolation => "preflop open/isolation".to_owned(),
        BetSizing::PotFraction(fraction) => format!("pot {fraction:.2}"),
        BetSizing::AllIn => "all-in".to_owned(),
    }
}

fn read_human_action(
    context: &BotDecisionContext,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> mtholdem::Result<HumanInput> {
    loop {
        write_human_prompt(context, output)?;
        let mut line = String::new();
        if input
            .read_line(&mut line)
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?
            == 0
        {
            return Err(mental_card_core::error::protocol(
                "human input ended before an action was entered",
            ));
        }
        match parse_human_action(line.trim(), context.legal_actions) {
            Ok(action) => return Ok(action),
            Err(error) => {
                writeln!(output, "invalid action: {error}")
                    .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
            }
        }
    }
}

fn write_human_prompt(
    context: &BotDecisionContext,
    output: &mut impl Write,
) -> mtholdem::Result<()> {
    writeln!(output).map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    writeln!(output, "{}: {} to act", context.street, context.actor)
        .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    if let Some([first, second]) = context.hole_cards {
        writeln!(output, "hand: {first} {second}")
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    } else {
        writeln!(output, "hand: unknown")
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    }
    writeln!(output, "board: {}", format_cards(&context.board))
        .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    writeln!(
        output,
        "to call: {}",
        context.legal_actions.to_call().units()
    )
    .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    if let Some(minimum) = context.legal_actions.minimum_raise_to() {
        let range_label = if is_open_bet(context.street, context.legal_actions) {
            "bet range"
        } else {
            "raise range"
        };
        writeln!(
            output,
            "{range_label}: {}..={}",
            minimum.units(),
            context.legal_actions.maximum_raise_to().units()
        )
        .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    } else {
        writeln!(output, "raise range: none")
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    }
    writeln!(
        output,
        "actions: {}",
        format_action_help(context.street, context.legal_actions)
    )
    .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    write!(output, "> ").map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
    output
        .flush()
        .map_err(|error| mental_card_core::error::protocol(error.to_string()))
}

fn parse_human_action(input: &str, legal: LegalActions) -> Result<HumanInput, String> {
    let mut parts = input.split_whitespace();
    let command = parts
        .next()
        .ok_or_else(|| "enter fold, check, call, bet N, raise N, or quit".to_owned())?
        .to_ascii_lowercase();
    let action = match command.as_str() {
        "q" | "quit" => HumanInput::Quit,
        "f" | "fold" => {
            if legal.can_check() {
                return Err("fold is not offered when check is legal".to_owned());
            }
            HumanInput::Action(BetAction::Fold)
        }
        "x" | "check" => {
            if !legal.can_check() {
                return Err(format!(
                    "check is not legal; call {}",
                    legal.to_call().units()
                ));
            }
            HumanInput::Action(BetAction::Check)
        }
        "c" | "call" => {
            if legal.can_check() {
                return Err("call is not legal when check is available".to_owned());
            }
            HumanInput::Action(BetAction::Call)
        }
        "b" | "bet" | "r" | "raise" => {
            let amount = parts
                .next()
                .ok_or_else(|| "bet/raise requires an amount".to_owned())?
                .parse()
                .map_err(|_| "bet/raise amount must be an integer chip value".to_owned())?;
            validate_raise(amount, legal)?;
            HumanInput::Action(BetAction::RaiseTo(Chips::from_units(amount)))
        }
        _ => {
            return Err(
                "unknown action; use fold, check, call, bet N, raise N, or quit".to_owned(),
            );
        }
    };
    if parts.next().is_some() {
        return Err("unexpected trailing input".to_owned());
    }
    Ok(action)
}

fn validate_raise(amount: u64, legal: LegalActions) -> Result<(), String> {
    let Some(minimum) = legal.minimum_raise_to() else {
        return Err("bet/raise is not currently legal".to_owned());
    };
    if amount < minimum.units() {
        return Err(format!("bet/raise must be at least {}", minimum.units()));
    }
    if amount > legal.maximum_raise_to().units() {
        return Err(format!(
            "bet/raise must be at most {}",
            legal.maximum_raise_to().units()
        ));
    }
    Ok(())
}

fn format_action_help(street: Street, legal: LegalActions) -> String {
    let mut actions = Vec::new();
    if legal.can_check() {
        actions.push("check".to_owned());
    } else {
        actions.push("fold".to_owned());
        actions.push("call".to_owned());
    }
    if let Some(minimum) = legal.minimum_raise_to() {
        let verb = if is_open_bet(street, legal) {
            "bet"
        } else {
            "raise"
        };
        actions.push(format!(
            "{verb} N ({}..={})",
            minimum.units(),
            legal.maximum_raise_to().units()
        ));
    }
    actions.push("quit".to_owned());
    actions.join(", ")
}

fn format_cards(cards: &[Card]) -> String {
    if cards.is_empty() {
        "-".to_owned()
    } else {
        cards
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn write_action(
    actor: Seat,
    street: Street,
    action: BetAction,
    legal: LegalActions,
    output: &mut impl Write,
) -> mtholdem::Result<()> {
    writeln!(output, "{}", format_action(actor, street, action, legal))
        .map_err(|error| mental_card_core::error::protocol(error.to_string()))
}

fn format_action(actor: Seat, street: Street, action: BetAction, legal: LegalActions) -> String {
    match action {
        BetAction::Fold => format!("{street}: {actor} folds"),
        BetAction::Check => format!("{street}: {actor} checks"),
        BetAction::Call => format!("{street}: {actor} calls"),
        BetAction::RaiseTo(amount) if is_open_bet(street, legal) => {
            format!("{street}: {actor} bets {}", amount.units())
        }
        BetAction::RaiseTo(amount) => format!("{street}: {actor} raises to {}", amount.units()),
        BetAction::AllIn => format!("{street}: {actor} moves all-in"),
    }
}

fn format_action_name(street: Street, action: BetAction, legal: LegalActions) -> String {
    match action {
        BetAction::Fold => "fold".to_owned(),
        BetAction::Check => "check".to_owned(),
        BetAction::Call => "call".to_owned(),
        BetAction::RaiseTo(amount) if is_open_bet(street, legal) => {
            format!("bet {}", amount.units())
        }
        BetAction::RaiseTo(amount) => format!("raise to {}", amount.units()),
        BetAction::AllIn => "all-in".to_owned(),
    }
}

fn is_open_bet(street: Street, legal: LegalActions) -> bool {
    street != Street::Preflop && legal.to_call().is_zero()
}

fn next_button(current: Seat, stacks: &[Chips]) -> Seat {
    for offset in 1..=stacks.len() {
        let index = (current.index() + offset) % stacks.len();
        if !stacks[index].is_zero() {
            return Seat::new(index).expect("stack index fits in a seat");
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_bot_finishes_one_hand_and_conserves_chips() {
        let mut stats = SessionStats::new(2, false);
        let result = finished_hand(
            run_hand(
                Options {
                    players: 2,
                    seed: 11,
                    preset: BotPreset::Balanced,
                    human: None,
                    hands: None,
                    backend: TableBackend::Peer,
                    stats: false,
                    watch_bot: None,
                },
                HandNumber::FIRST,
                &TableState::new(2),
                &mut stats,
                &mut io::empty(),
                &mut io::sink(),
            )
            .expect("bot hand finishes"),
        );

        assert_eq!(
            result
                .final_stacks()
                .iter()
                .map(|stack| stack.units())
                .sum::<u64>(),
            200
        );
    }

    #[test]
    fn play_bot_accepts_human_actions_from_input() {
        let script = "call\ncheck\n".repeat(20);
        let mut input = script.as_bytes();
        let mut output = Vec::new();
        let mut stats = SessionStats::new(2, false);
        let result = finished_hand(
            run_hand(
                Options {
                    players: 2,
                    seed: 11,
                    preset: BotPreset::Balanced,
                    human: Some(Seat::new(0).expect("seat")),
                    hands: None,
                    backend: TableBackend::Peer,
                    stats: false,
                    watch_bot: None,
                },
                HandNumber::FIRST,
                &TableState::new(2),
                &mut stats,
                &mut input,
                &mut output,
            )
            .expect("human hand finishes"),
        );

        assert_eq!(
            result
                .final_stacks()
                .iter()
                .map(|stack| stack.units())
                .sum::<u64>(),
            200
        );
        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("hand:"));
        assert!(output.contains("actions:"));
    }

    #[test]
    fn play_bot_prints_board_updates_after_human_fold() {
        let mut input = "fold\n".as_bytes();
        let mut output = Vec::new();
        let outcome = run_session(
            Options {
                players: 3,
                seed: 29,
                preset: BotPreset::Balanced,
                human: Some(Seat::new(0).expect("seat")),
                hands: Some(1),
                backend: TableBackend::RulesOnly,
                stats: false,
                watch_bot: None,
            },
            &mut input,
            &mut output,
        )
        .expect("folded human can observe hand");

        assert_eq!(outcome.completed_hands, 1);
        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("flop:"));
        assert!(output.contains("turn:"));
        assert!(output.contains("river:"));
    }

    #[test]
    fn bots_only_prints_board_updates() {
        let mut output = Vec::new();
        let outcome = run_session(
            Options {
                players: 3,
                seed: 1,
                preset: BotPreset::Maniac,
                human: None,
                hands: Some(1),
                backend: TableBackend::RulesOnly,
                stats: false,
                watch_bot: None,
            },
            &mut io::empty(),
            &mut output,
        )
        .expect("bots-only hand finishes");

        assert_eq!(outcome.completed_hands, 1);
        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("flop:"));
        assert!(output.contains("turn:"));
        assert!(output.contains("river:"));
    }

    #[test]
    fn play_bot_config_supports_larger_tables() {
        let mut stats = SessionStats::new(4, false);
        let result = finished_hand(
            run_hand(
                Options {
                    players: 4,
                    seed: 44,
                    preset: BotPreset::Nit,
                    human: None,
                    hands: None,
                    backend: TableBackend::Peer,
                    stats: false,
                    watch_bot: None,
                },
                HandNumber::FIRST,
                &TableState::new(4),
                &mut stats,
                &mut io::empty(),
                &mut io::sink(),
            )
            .expect("four-player bot hand finishes"),
        );

        assert_eq!(
            result
                .final_stacks()
                .iter()
                .map(|stack| stack.units())
                .sum::<u64>(),
            400
        );
    }

    #[test]
    fn table_active_player_count_excludes_busted_seats() {
        let mut table = TableState::new(4);
        table.stacks[1] = Chips::ZERO;

        assert_eq!(table.active_player_count(), 3);
        assert!(table.can_continue());
    }

    #[test]
    fn play_bot_runs_bounded_human_sessions() {
        let script = "call\ncheck\n".repeat(50);
        let mut input = script.as_bytes();
        let mut output = Vec::new();
        let outcome = run_session(
            Options {
                players: 2,
                seed: 11,
                preset: BotPreset::Balanced,
                human: Some(Seat::new(0).expect("seat")),
                hands: Some(2),
                backend: TableBackend::Peer,
                stats: false,
                watch_bot: None,
            },
            &mut input,
            &mut output,
        )
        .expect("bounded session finishes");

        assert_eq!(outcome.completed_hands, 2);
        assert!(!outcome.quit);
    }

    #[test]
    fn play_bot_quits_from_human_prompt() {
        let mut input = "q\n".as_bytes();
        let mut output = Vec::new();
        let outcome = run_session(
            Options {
                players: 2,
                seed: 11,
                preset: BotPreset::Balanced,
                human: Some(Seat::new(0).expect("seat")),
                hands: None,
                backend: TableBackend::Peer,
                stats: false,
                watch_bot: None,
            },
            &mut input,
            &mut output,
        )
        .expect("quit exits session");

        assert_eq!(outcome.completed_hands, 0);
        assert!(outcome.quit);
    }

    #[test]
    fn bots_only_defaults_to_one_hand() {
        let mut output = Vec::new();
        let outcome = run_session(
            Options {
                players: 2,
                seed: 11,
                preset: BotPreset::Balanced,
                human: None,
                hands: None,
                backend: TableBackend::Peer,
                stats: false,
                watch_bot: None,
            },
            &mut io::empty(),
            &mut output,
        )
        .expect("bots-only default finishes");

        assert_eq!(outcome.completed_hands, 1);
        assert!(!outcome.quit);
    }

    #[test]
    fn bots_only_respects_hands_limit() {
        let mut output = Vec::new();
        let outcome = run_session(
            Options {
                players: 2,
                seed: 11,
                preset: BotPreset::Balanced,
                human: None,
                hands: Some(2),
                backend: TableBackend::Peer,
                stats: false,
                watch_bot: None,
            },
            &mut io::empty(),
            &mut output,
        )
        .expect("bots-only bounded finishes");

        assert_eq!(outcome.completed_hands, 2);
        assert!(!outcome.quit);
    }

    #[test]
    fn peer_backend_carries_final_stacks_to_next_hand() {
        let mut output = Vec::new();
        let outcome = run_session(
            Options {
                players: 3,
                seed: 1,
                preset: BotPreset::Balanced,
                human: None,
                hands: Some(2),
                backend: TableBackend::Peer,
                stats: false,
                watch_bot: None,
            },
            &mut io::empty(),
            &mut output,
        )
        .expect("two peer-backed hands finish");

        assert_eq!(outcome.completed_hands, 2);
        let output = String::from_utf8(output).expect("utf8");
        let starting_stacks = output
            .lines()
            .filter_map(|line| line.strip_prefix("starting stacks: "))
            .collect::<Vec<_>>();
        let final_stacks = output
            .lines()
            .filter_map(|line| line.strip_prefix("final stacks: "))
            .collect::<Vec<_>>();
        assert_eq!(starting_stacks.len(), 2);
        assert_eq!(final_stacks.len(), 2);
        let second_start = starting_stacks[1]
            .split(" button=")
            .next()
            .expect("starting stack line has stack segment");
        assert_eq!(second_start, final_stacks[0]);
        assert_eq!(
            outcome
                .final_stacks
                .iter()
                .map(|stack| stack.units())
                .sum::<u64>(),
            300
        );
    }

    #[test]
    fn rules_only_backend_runs_and_carries_stacks() {
        let mut output = Vec::new();
        let outcome = run_session(
            Options {
                players: 3,
                seed: 77,
                preset: BotPreset::Balanced,
                human: None,
                hands: Some(2),
                backend: TableBackend::RulesOnly,
                stats: false,
                watch_bot: None,
            },
            &mut io::empty(),
            &mut output,
        )
        .expect("two rules-only hands finish");

        assert_eq!(outcome.completed_hands, 2);
        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("backend=rules-only"));
        let starting_stacks = output
            .lines()
            .filter_map(|line| line.strip_prefix("starting stacks: "))
            .collect::<Vec<_>>();
        let final_stacks = output
            .lines()
            .filter_map(|line| line.strip_prefix("final stacks: "))
            .collect::<Vec<_>>();
        let second_start = starting_stacks[1]
            .split(" button=")
            .next()
            .expect("starting stack line has stack segment");
        assert_eq!(second_start, final_stacks[0]);
        assert_eq!(
            outcome
                .final_stacks
                .iter()
                .map(|stack| stack.units())
                .sum::<u64>(),
            300
        );
    }

    #[test]
    fn bots_only_stats_print_preflop_rates() {
        let mut output = Vec::new();
        let outcome = run_session(
            Options {
                players: 3,
                seed: 7,
                preset: BotPreset::Balanced,
                human: None,
                hands: Some(2),
                backend: TableBackend::RulesOnly,
                stats: true,
                watch_bot: None,
            },
            &mut io::empty(),
            &mut output,
        )
        .expect("stats session finishes");

        assert_eq!(outcome.completed_hands, 2);
        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("bot action stats:"));
        assert!(output.contains("vpip pfr rfi 3bet fold-to-open cold-continue blind-defense"));
        assert!(output.contains("dealt-player action stats:"));
        assert!(output.contains("p3 6 "));
        assert!(output.contains("postflop action stats:"));
        assert!(output.contains("seat actions bet raise call fold check"));
        assert!(output.contains("postflop by dealt players:"));
        assert!(output.contains("flop continuation-bet stats:"));
        assert!(output.contains("flop continuation-bet by dealt players:"));
        assert!(output.contains("turn barrel stats:"));
        assert!(output.contains("turn barrel by dealt players:"));
        assert!(output.contains("all 6 "));
        assert!(!output.contains("=== hand"));
        assert!(!output.contains("starting stacks:"));
    }

    #[test]
    fn watch_bot_prints_decision_details() {
        let mut output = Vec::new();
        let outcome = run_session(
            Options {
                players: 3,
                seed: 7,
                preset: BotPreset::Balanced,
                human: None,
                hands: Some(1),
                backend: TableBackend::RulesOnly,
                stats: false,
                watch_bot: Some(Seat::new(0).expect("seat")),
            },
            &mut io::empty(),
            &mut output,
        )
        .expect("watched bot session finishes");

        assert_eq!(outcome.completed_hands, 1);
        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("bot-watch: seat 0"));
        assert!(output.contains("hand:"));
        assert!(output.contains("assessment:"));
        assert!(output.contains("decision:"));
        assert!(output.contains("preflop: seat 0"));
        assert!(!output.contains("action=preflop:"));
    }

    #[test]
    fn human_action_parser_rejects_illegal_raise_amounts() {
        let legal = BotDecisionContext::new(
            2,
            Seat::new(0).expect("seat"),
            Street::Preflop,
            test_legal_actions(),
        )
        .legal_actions;

        assert!(parse_human_action("raise 3", legal).is_err());
        assert_eq!(parse_human_action("q", legal), Ok(HumanInput::Quit));
    }

    #[test]
    fn human_action_parser_accepts_bet_alias() {
        let legal = test_open_bet_actions();

        assert_eq!(
            parse_human_action("bet 2", legal),
            Ok(HumanInput::Action(BetAction::RaiseTo(Chips::from_units(2))))
        );
        assert!(format_action_help(Street::Flop, legal).contains("bet N"));
        assert!(
            format_action_help(Street::Preflop, legal).contains("raise N"),
            "preflop blind raises should not be presented as open bets"
        );
    }

    #[test]
    fn action_format_distinguishes_open_bets_from_raises() {
        let actor = Seat::new(0).expect("seat");
        let open_bet = test_open_bet_actions();
        let facing_bet = test_legal_actions();

        assert_eq!(
            format_action(
                actor,
                Street::Flop,
                BetAction::RaiseTo(Chips::from_units(2)),
                open_bet,
            ),
            "flop: seat 0 bets 2"
        );
        assert_eq!(
            format_action(
                actor,
                Street::Preflop,
                BetAction::RaiseTo(Chips::from_units(4)),
                facing_bet,
            ),
            "preflop: seat 0 raises to 4"
        );
    }

    #[test]
    fn showdown_summary_labels_opened_hand_categories() {
        let board = [
            Card::new(Rank::Ace, Suit::Hearts),
            Card::new(Rank::Four, Suit::Clubs),
            Card::new(Rank::Four, Suit::Spades),
            Card::new(Rank::Three, Suit::Hearts),
            Card::new(Rank::Six, Suit::Diamonds),
        ];
        let showdown = TableShowdown::from_opened(
            &board,
            [
                (
                    Seat::new(1).expect("seat"),
                    [
                        Card::new(Rank::Eight, Suit::Spades),
                        Card::new(Rank::Ten, Suit::Spades),
                    ],
                ),
                (
                    Seat::new(2).expect("seat"),
                    [
                        Card::new(Rank::Ten, Suit::Hearts),
                        Card::new(Rank::Ace, Suit::Spades),
                    ],
                ),
            ],
        )
        .expect("showdown summary");

        let mut output = Vec::new();
        write_showdown_summary(&showdown, &mut output).expect("showdown summary");

        assert_eq!(
            String::from_utf8(output).expect("utf8"),
            "showdown: seat1=8s Ts (one pair) seat2=Th As (two pair)\n"
        );
    }

    #[test]
    fn play_bot_defaults_to_human_seat_zero() {
        let options = parse_options(&[]).expect("default options");
        assert_eq!(options.human, Some(Seat::new(0).expect("seat")));
    }

    #[test]
    fn play_bot_can_disable_human_input() {
        let options = parse_options(&["--bots-only".to_owned()]).expect("bots-only options");
        assert_eq!(options.human, None);
    }

    #[test]
    fn play_bot_can_watch_bot_seat() {
        let options =
            parse_options(&["--watch-bot".to_owned(), "seat0".to_owned()]).expect("watch options");
        assert_eq!(options.watch_bot, Some(Seat::new(0).expect("seat")));
        assert_eq!(options.human, None);
    }

    #[test]
    fn play_bot_can_watch_numeric_bot_seat() {
        let options =
            parse_options(&["--watch-bot".to_owned(), "0".to_owned()]).expect("watch options");
        assert_eq!(options.watch_bot, Some(Seat::new(0).expect("seat")));
    }

    #[test]
    fn play_bot_rejects_out_of_range_watch_bot() {
        assert!(
            parse_options(&[
                "--players".to_owned(),
                "3".to_owned(),
                "--watch-bot".to_owned(),
                "3".to_owned(),
            ])
            .is_err()
        );
    }

    #[test]
    fn play_bot_rejects_human_and_watch_bot_combination() {
        assert!(
            parse_options(&[
                "--human".to_owned(),
                "0".to_owned(),
                "--watch-bot".to_owned(),
                "1".to_owned(),
            ])
            .is_err()
        );
    }

    #[test]
    fn play_bot_can_select_rules_only_backend() {
        let options = parse_options(&["--backend".to_owned(), "rules-only".to_owned()])
            .expect("rules-only options");
        assert_eq!(options.backend, TableBackend::RulesOnly);
    }

    #[test]
    fn play_bot_can_enable_stats() {
        let options = parse_options(&["--stats".to_owned()]).expect("stats options");
        assert!(options.stats);
    }

    #[test]
    fn play_bot_rejects_zero_hands() {
        assert!(parse_options(&["--hands".to_owned(), "0".to_owned()]).is_err());
    }

    fn finished_hand(outcome: HandOutcome) -> FinishedHand {
        let HandOutcome::Finished(result) = outcome else {
            panic!("expected a finished hand");
        };
        result.finished
    }

    fn test_legal_actions() -> LegalActions {
        let stacks = [Chips::from_units(100), Chips::from_units(100)];
        let hand = HoldemHand::new(
            &stacks,
            Seat::new(0).expect("seat"),
            Chips::from_units(1),
            Chips::from_units(2),
            Chips::from_units(100),
        )
        .expect("hand");
        hand.legal_actions().expect("legal")
    }

    fn test_open_bet_actions() -> LegalActions {
        let stacks = [Chips::from_units(100), Chips::from_units(100)];
        let mut hand = HoldemHand::new(
            &stacks,
            Seat::new(0).expect("seat"),
            Chips::from_units(1),
            Chips::from_units(2),
            Chips::from_units(100),
        )
        .expect("hand");
        hand.apply_action(Seat::new(0).expect("seat"), BetAction::Call)
            .expect("small blind call");
        hand.apply_action(Seat::new(1).expect("seat"), BetAction::Check)
            .expect("big blind check");
        hand.open_flop([
            Card::new(Rank::Two, Suit::Clubs),
            Card::new(Rank::Seven, Suit::Diamonds),
            Card::new(Rank::Nine, Suit::Hearts),
        ])
        .expect("open flop");
        hand.legal_actions().expect("postflop legal")
    }
}
