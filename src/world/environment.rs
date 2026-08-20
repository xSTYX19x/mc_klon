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
