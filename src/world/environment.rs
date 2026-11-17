#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum WorldEnvironment {
    Temperate,
    Winter,
}

impl WorldEnvironment {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Temperate => "Temperate",
            Self::Winter => "Winter",
        }
    }

    pub(crate) const fn meta_key(self) -> &'static str {
        match self {
            Self::Temperate => "temperate",
            Self::Winter => "winter",
        }
    }

    pub(crate) fn from_meta_key(raw: &str) -> Option<Self> {
        match raw {
            "temperate" => Some(Self::Temperate),
            "winter" => Some(Self::Winter),
            _ => None,
        }
    }

    pub(crate) const fn default_precipitation(self) -> Option<PrecipitationKind> {
        match self {
            Self::Temperate => Some(PrecipitationKind::Rain),
            Self::Winter => Some(PrecipitationKind::Snow),
        }
    }

    pub(crate) const fn enables_winter_surface_rules(self) -> bool {
        matches!(self, Self::Winter)
    }
}

impl Default for WorldEnvironment {
    fn default() -> Self {
        Self::Temperate
    }
}

/// What "Generate new level" asks for. `Random`, the default, makes a quarter
/// of all levels winter like Alpha did, decided by the level seed so the same
/// seed always gives the same world.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum EnvironmentChoice {
    #[default]
    Random,
    Temperate,
    Winter,
}

const WINTER_SEED_SALT: u64 = 0x57A1_7E2C_0B5E_EDD5;

impl EnvironmentChoice {
    pub(crate) const ALL: [Self; 3] = [Self::Random, Self::Temperate, Self::Winter];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Random => "Random (1 in 4 winter)",
            Self::Temperate => "Temperate",
            Self::Winter => "Winter",
        }
    }

    pub(crate) fn resolve(self, seed: u64) -> WorldEnvironment {
        match self {
            Self::Random if winter_for_seed(seed) => WorldEnvironment::Winter,
            Self::Random | Self::Temperate => WorldEnvironment::Temperate,
            Self::Winter => WorldEnvironment::Winter,
        }
    }
}

/// A quarter of all seeds are winter seeds.
fn winter_for_seed(seed: u64) -> bool {
    crate::util::rng::hash64(seed ^ WINTER_SEED_SALT).is_multiple_of(4)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PrecipitationKind {
    Rain,
    Snow,
}

impl PrecipitationKind {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Rain => "rain",
            Self::Snow => "snow",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_choice_makes_a_quarter_of_seeds_winter_and_sticks_to_the_seed() {
        let seeds = 0..20_000_u64;
        let winter = seeds
            .clone()
            .filter(|&seed| EnvironmentChoice::Random.resolve(seed) == WorldEnvironment::Winter)
            .count();
        let share = winter as f64 / seeds.count() as f64;
        assert!((0.24..=0.26).contains(&share), "winter share {share}");
        for seed in [0, 1, 42, u64::MAX] {
            assert_eq!(
                EnvironmentChoice::Random.resolve(seed),
                EnvironmentChoice::Random.resolve(seed)
            );
        }
        for seed in 0..64 {
            assert_eq!(
                EnvironmentChoice::Temperate.resolve(seed),
                WorldEnvironment::Temperate
            );
            assert_eq!(EnvironmentChoice::Winter.resolve(seed), WorldEnvironment::Winter);
        }
    }
}
