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
use mtholdem_bot::{BotDecisionContext, BotPreset, HeuristicBot};
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
        self.stacks.iter().filter(|stack| !stack.is_zero()).count() >= 2
            && self.maximum_stack() >= Chips::from_units(2)
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
    bot: HeuristicBot,
    bot_rng: StdRng,
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
            "--human" => {
                index += 1;
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
        "usage: mtholdem play-bot [--players N] [--seed N] [--bot PRESET] [--human SEAT] [--hands N] [--bots-only] [--backend peer|rules-only]"
    );
    println!("presets: nit, balanced, loose-passive, lag, maniac");
    println!("human seat defaults to seat0 and runs until quit; use --hands N to bound it");
    println!("--bots-only runs one hand by default; combine with --hands N for more");
    println!("human seats: 0, 1, ... or seat0, seat1, ...");
    println!("human actions: fold, check, call, bet N, raise N, quit");
    println!("backends: peer, rules-only");
    println!("board updates are printed as flop, turn, and river when cards open");
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
    outcome.final_stacks.clone_from(&table.stacks);
    loop {
        if hand_limit.is_some_and(|limit| outcome.completed_hands >= limit) {
            return Ok(outcome);
        }
        if !table.can_continue() {
            return Ok(outcome);
        }
        let hand_number = HandNumber::from_u64(outcome.completed_hands);
        writeln!(output, "=== hand {} ===", hand_number.to_u64())
            .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
        write_table_state(options.backend, &table, output)?;
        match run_hand(options, hand_number, &table, input, output)? {
            HandOutcome::Finished(result) => {
                outcome.completed_hands += 1;
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
                writeln!(output)
                    .map_err(|error| mental_card_core::error::protocol(error.to_string()))?;
                table.advance(result.finished.final_stacks());
                outcome.final_stacks.clone_from(&table.stacks);
            }
            HandOutcome::Quit => {
                outcome.quit = true;
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

fn run_hand(
    options: Options,
    hand_number: HandNumber,
    table: &TableState,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> mtholdem::Result<HandOutcome> {
    match options.backend {
        TableBackend::Peer => run_peer_hand(options, hand_number, table, input, output),
        TableBackend::RulesOnly => run_rules_only_hand(options, hand_number, table, input, output),
    }
}

fn run_peer_hand(
    options: Options,
    hand_number: HandNumber,
    table: &TableState,
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
    let active_seats = (0..options.players)
        .map(|index| Seat::new(index).expect("player index was range-checked"))
        .collect::<Vec<_>>();
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
        bot: HeuristicBot::from_preset(options.preset),
        bot_rng: StdRng::seed_from_u64(options.seed),
        crypto_rng,
    }
    .run(options.human, input, output)
}

fn run_rules_only_hand(
    options: Options,
    hand_number: HandNumber,
    table: &TableState,
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
                        .with_hole_cards(hole_cards[actor.index()])
                        .with_board(hand.board().to_vec())
                        .with_action_context(action_context);
                let action = if options.human == Some(actor) {
                    read_human_action(&context, input, output)?
                } else {
                    HumanInput::Action(bot.choose_action(&context, &mut rng))
                };
                let HumanInput::Action(action) = action else {
                    return Ok(HandOutcome::Quit);
                };
                print_action(actor, street, action, legal_actions);
                hand.apply_action(actor, action)?;
            }
            HandProgress::AwaitingFlop => {
                let flop = [
                    draw_card(&mut deck)?,
                    draw_card(&mut deck)?,
                    draw_card(&mut deck)?,
                ];
                hand.open_flop(flop)?;
                write_board_line("flop", hand.board(), output)?;
            }
            HandProgress::AwaitingTurn => {
                hand.open_turn(draw_card(&mut deck)?)?;
                write_board_line("turn", hand.board(), output)?;
            }
            HandProgress::AwaitingRiver => {
                hand.open_river(draw_card(&mut deck)?)?;
                write_board_line("river", hand.board(), output)?;
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
        input: &mut impl BufRead,
        output: &mut impl Write,
    ) -> mtholdem::Result<HandOutcome> {
        for index in 0..self.sessions.len() {
            if self.progress_and_handle(index, human, input, output)? {
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
                        if self.progress_and_handle(index, human, input, output)? {
                            return Ok(HandOutcome::Quit);
                        }
                    }
                }
                QueuedEffect::Private { recipient, bytes } => {
                    let index = recipient.index();
                    self.sessions[index].apply_private_bytes(&bytes)?;
                    if self.progress_and_handle(index, human, input, output)? {
                        return Ok(HandOutcome::Quit);
                    }
                }
                QueuedEffect::Showdown { owner, bundle } => {
                    for index in 0..self.sessions.len() {
                        if index == owner.index() {
                            continue;
                        }
                        self.sessions[index].apply_showdown_bundle((*bundle).clone())?;
                        if self.progress_and_handle(index, human, input, output)? {
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
        input: &mut impl BufRead,
        output: &mut impl Write,
    ) -> mtholdem::Result<bool> {
        self.sessions[index].progress(&mut self.crypto_rng)?;
        self.write_board_updates(output)?;
        loop {
            let effects = self.sessions[index].drain_effects();
            if effects.is_empty() {
                return Ok(false);
            }
            for effect in effects {
                if self.handle_effect(index, effect, human, input, output)? {
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
                    BotDecisionContext::new(self.sessions.len(), actor, street, legal_actions)
                        .with_hole_cards(self.sessions[sender_index].local_hole_cards())
                        .with_board(board_cards(&self.sessions[sender_index]))
                        .with_action_context(action_context);
                let action = if human == Some(actor) {
                    read_human_action(&context, input, output)?
                } else {
                    HumanInput::Action(self.bot.choose_action(&context, &mut self.bot_rng))
                };
                let HumanInput::Action(action) = action else {
                    return Ok(true);
                };
                print_action(actor, street, action, legal_actions);
                self.sessions[sender_index].submit_action(action)?;
                self.sessions[sender_index].progress(&mut self.crypto_rng)?;
                self.write_board_updates(output)?;
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

fn print_action(actor: Seat, street: Street, action: BetAction, legal: LegalActions) {
    println!("{}", format_action(actor, street, action, legal));
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
        let result = finished_hand(
            run_hand(
                Options {
                    players: 2,
                    seed: 11,
                    preset: BotPreset::Balanced,
                    human: None,
                    hands: None,
                    backend: TableBackend::Peer,
                },
                HandNumber::FIRST,
                &TableState::new(2),
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
        let result = finished_hand(
            run_hand(
                Options {
                    players: 2,
                    seed: 11,
                    preset: BotPreset::Balanced,
                    human: Some(Seat::new(0).expect("seat")),
                    hands: None,
                    backend: TableBackend::Peer,
                },
                HandNumber::FIRST,
                &TableState::new(2),
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
                seed: 12,
                preset: BotPreset::Balanced,
                human: Some(Seat::new(0).expect("seat")),
                hands: Some(1),
                backend: TableBackend::RulesOnly,
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
                seed: 2,
                preset: BotPreset::Balanced,
                human: None,
                hands: Some(1),
                backend: TableBackend::RulesOnly,
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
        let result = finished_hand(
            run_hand(
                Options {
                    players: 4,
                    seed: 44,
                    preset: BotPreset::Nit,
                    human: None,
                    hands: None,
                    backend: TableBackend::Peer,
                },
                HandNumber::FIRST,
                &TableState::new(4),
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
    fn play_bot_can_select_rules_only_backend() {
        let options = parse_options(&["--backend".to_owned(), "rules-only".to_owned()])
            .expect("rules-only options");
        assert_eq!(options.backend, TableBackend::RulesOnly);
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
