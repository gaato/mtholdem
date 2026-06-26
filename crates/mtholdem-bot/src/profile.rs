/// A configurable lightweight bot tendency profile.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BotProfile {
    /// Wider values enter more pots.
    pub looseness: f32,
    /// Higher values prefer raises and bets.
    pub aggression: f32,
    /// Higher values prefer calls over folds or raises.
    pub call_bias: f32,
    /// Chance-like weight for betting weak holdings.
    pub bluff_rate: f32,
    /// Amount of random action-preference jitter.
    pub randomness: f32,
}

impl BotProfile {
    /// Creates a profile from raw tendency values.
    #[must_use]
    pub const fn new(
        looseness: f32,
        aggression: f32,
        call_bias: f32,
        bluff_rate: f32,
        randomness: f32,
    ) -> Self {
        Self {
            looseness,
            aggression,
            call_bias,
            bluff_rate,
            randomness,
        }
    }

    /// Returns this profile with all values clamped to `0.0..=1.0`.
    #[must_use]
    pub fn clamped(self) -> Self {
        Self {
            looseness: clamp01(self.looseness),
            aggression: clamp01(self.aggression),
            call_bias: clamp01(self.call_bias),
            bluff_rate: clamp01(self.bluff_rate),
            randomness: clamp01(self.randomness),
        }
    }
}

impl Default for BotProfile {
    fn default() -> Self {
        BotPreset::Balanced.profile()
    }
}

/// Named bot tendency presets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BotPreset {
    /// Very tight and not eager to fight for marginal pots.
    Nit,
    /// Moderate ranges and aggression.
    Balanced,
    /// Enters too many pots but rarely raises.
    LoosePassive,
    /// Loose and aggressive.
    Lag,
    /// Very loose, volatile, and bluff-heavy.
    Maniac,
}

impl BotPreset {
    /// Returns the profile for this preset.
    #[must_use]
    pub const fn profile(self) -> BotProfile {
        match self {
            Self::Nit => BotProfile::new(0.18, 0.32, 0.20, 0.01, 0.06),
            Self::Balanced => BotProfile::new(0.34, 0.52, 0.34, 0.05, 0.10),
            Self::LoosePassive => BotProfile::new(0.56, 0.22, 0.68, 0.02, 0.12),
            Self::Lag => BotProfile::new(0.56, 0.74, 0.32, 0.11, 0.16),
            Self::Maniac => BotProfile::new(0.72, 0.90, 0.38, 0.22, 0.22),
        }
    }
}

const fn clamp01(value: f32) -> f32 {
    if value.is_nan() || value < 0.0 {
        0.0
    } else if value > 1.0 {
        1.0
    } else {
        value
    }
}
