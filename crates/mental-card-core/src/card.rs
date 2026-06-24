//! Standard 52-card deck values shared by dealerless card games.
//!
//! This module owns only stable card identity and notation.  It deliberately does not
//! evaluate poker hands or encode any particular game's rules.  The suit-major index
//! mapping is consensus-critical because [`crate::crypto`] encrypts those indices.

use std::fmt;
use std::str::FromStr;

use crate::error::{Result, rules};
use crate::types::DECK_SIZE;

/// The four suits in the standard deck.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum Suit {
    /// Clubs (`c`).
    Clubs = 0,
    /// Diamonds (`d`).
    Diamonds = 1,
    /// Hearts (`h`).
    Hearts = 2,
    /// Spades (`s`).
    Spades = 3,
}

impl Suit {
    /// Returns the compact ASCII character used by poker notation.
    #[must_use]
    pub const fn symbol(self) -> char {
        match self {
            Self::Clubs => 'c',
            Self::Diamonds => 'd',
            Self::Hearts => 'h',
            Self::Spades => 's',
        }
    }

    fn from_symbol(symbol: char) -> Option<Self> {
        match symbol.to_ascii_lowercase() {
            'c' => Some(Self::Clubs),
            'd' => Some(Self::Diamonds),
            'h' => Some(Self::Hearts),
            's' => Some(Self::Spades),
            _ => None,
        }
    }
}

impl fmt::Display for Suit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.symbol().fmt(formatter)
    }
}

/// A card rank from deuce through ace.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum Rank {
    /// Two.
    Two = 2,
    /// Three.
    Three = 3,
    /// Four.
    Four = 4,
    /// Five.
    Five = 5,
    /// Six.
    Six = 6,
    /// Seven.
    Seven = 7,
    /// Eight.
    Eight = 8,
    /// Nine.
    Nine = 9,
    /// Ten.
    Ten = 10,
    /// Jack.
    Jack = 11,
    /// Queen.
    Queen = 12,
    /// King.
    King = 13,
    /// Ace.
    Ace = 14,
}

impl Rank {
    /// Returns the numeric poker value in `2..=14`.
    #[must_use]
    pub const fn value(self) -> u8 {
        self as u8
    }

    /// Returns the compact ASCII symbol used by poker notation.
    #[must_use]
    pub const fn symbol(self) -> char {
        match self {
            Self::Two => '2',
            Self::Three => '3',
            Self::Four => '4',
            Self::Five => '5',
            Self::Six => '6',
            Self::Seven => '7',
            Self::Eight => '8',
            Self::Nine => '9',
            Self::Ten => 'T',
            Self::Jack => 'J',
            Self::Queen => 'Q',
            Self::King => 'K',
            Self::Ace => 'A',
        }
    }

    fn from_symbol(symbol: char) -> Option<Self> {
        match symbol.to_ascii_uppercase() {
            '2' => Some(Self::Two),
            '3' => Some(Self::Three),
            '4' => Some(Self::Four),
            '5' => Some(Self::Five),
            '6' => Some(Self::Six),
            '7' => Some(Self::Seven),
            '8' => Some(Self::Eight),
            '9' => Some(Self::Nine),
            'T' => Some(Self::Ten),
            'J' => Some(Self::Jack),
            'Q' => Some(Self::Queen),
            'K' => Some(Self::King),
            'A' => Some(Self::Ace),
            _ => None,
        }
    }

    fn from_value(value: u8) -> Option<Self> {
        match value {
            2 => Some(Self::Two),
            3 => Some(Self::Three),
            4 => Some(Self::Four),
            5 => Some(Self::Five),
            6 => Some(Self::Six),
            7 => Some(Self::Seven),
            8 => Some(Self::Eight),
            9 => Some(Self::Nine),
            10 => Some(Self::Ten),
            11 => Some(Self::Jack),
            12 => Some(Self::Queen),
            13 => Some(Self::King),
            14 => Some(Self::Ace),
            _ => None,
        }
    }
}

impl fmt::Display for Rank {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.symbol().fmt(formatter)
    }
}

/// A standard playing card.
///
/// Card indices used by the cryptographic deck are stable and suit-major:
/// `2c..Ac, 2d..Ad, 2h..Ah, 2s..As` correspond to indices `0..52`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Card {
    rank: Rank,
    suit: Suit,
}

impl Card {
    /// Creates a card from a rank and suit.
    #[must_use]
    pub const fn new(rank: Rank, suit: Suit) -> Self {
        Self { rank, suit }
    }

    /// Returns the card's rank.
    #[must_use]
    pub const fn rank(self) -> Rank {
        self.rank
    }

    /// Returns the card's suit.
    #[must_use]
    pub const fn suit(self) -> Suit {
        self.suit
    }

    /// Converts the card to the stable cryptographic deck index in `0..52`.
    #[must_use]
    pub const fn to_index(self) -> usize {
        (self.suit as usize) * 13 + (self.rank as usize - 2)
    }

    /// Converts a stable cryptographic deck index into a card.
    pub fn from_index(index: usize) -> Result<Self> {
        if index >= DECK_SIZE {
            return Err(rules(format!(
                "card index {index} is outside 0..{DECK_SIZE}"
            )));
        }
        let suit = match index / 13 {
            0 => Suit::Clubs,
            1 => Suit::Diamonds,
            2 => Suit::Hearts,
            3 => Suit::Spades,
            _ => unreachable!("index was checked against DECK_SIZE"),
        };
        let rank = Rank::from_value(u8::try_from(index % 13 + 2).expect("rank fits u8"))
            .expect("index produces a valid rank");
        Ok(Self { rank, suit })
    }
}

impl fmt::Display for Card {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}{}", self.rank, self.suit)
    }
}

impl FromStr for Card {
    type Err = crate::Error;

    /// Parses two-character poker notation such as `As`, `Td`, or `2c`.
    fn from_str(text: &str) -> Result<Self> {
        let mut chars = text.chars();
        let rank = chars
            .next()
            .and_then(Rank::from_symbol)
            .ok_or_else(|| rules(format!("invalid card rank in {text:?}")))?;
        let suit = chars
            .next()
            .and_then(Suit::from_symbol)
            .ok_or_else(|| rules(format!("invalid card suit in {text:?}")))?;
        if chars.next().is_some() {
            return Err(rules(format!(
                "card notation must contain exactly two characters: {text:?}"
            )));
        }
        Ok(Self { rank, suit })
    }
}
