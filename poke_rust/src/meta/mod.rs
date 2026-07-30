//! Competitive usage statistics from championsbattledata.com.
//!
//! `meta_scraper/update_meta.py` stores the API data.
//! This module parses the cache into a `MetaDex`.
//! See `meta_scraper/README.md` for layout and attribution.
//!
//! This module performs file access and name resolution.
//! Therefore, it does not belong in the generated `data` module.

pub mod dex;
pub mod names;
pub mod schema;
pub mod team_gen;

pub use dex::{MetaDex, MetaError, MetaWarning, SpeciesMeta, StatPoints, Weighted};
pub use team_gen::{GeneratedSet, TeamGenError, generate_meta_team, render_teamsheet};

/// Which of the site's two per-format tables to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetaFormat {
    Singles,
    Doubles,
}

impl MetaFormat {
    /// Champions singles is one active Pokemon per side; anything else is
    /// doubles, matching `BattleState::active_per_side`.
    pub fn from_active_per_side(active_per_side: u8) -> MetaFormat {
        if active_per_side <= 1 {
            MetaFormat::Singles
        } else {
            MetaFormat::Doubles
        }
    }

    /// The on-disk directory name, which is also the site's own format label.
    pub fn dir_name(self) -> &'static str {
        match self {
            MetaFormat::Singles => "Singles",
            MetaFormat::Doubles => "Doubles",
        }
    }
}
