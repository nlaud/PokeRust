use crate::data::ability::Ability;
use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::state::dex_data::{
    MoveData, PokemonBoostTable, PokemonData, PokemonType, Status, VolatileStatus, parse_type,
};
use std::collections::HashMap;
use std::fs;

pub type PokemonStatsTable = [u16; 6]; // hp, atk, def, spa, spd, spe

fn humanize_identifier(value: impl AsRef<str>) -> String {
    let value = value.as_ref();
    let mut result = String::new();
    let mut previous: Option<char> = None;

    for current in value.chars() {
        let insert_space = match previous {
            Some(prev) => {
                (prev.is_ascii_lowercase() && current.is_ascii_uppercase())
                    || (prev.is_ascii_digit() && current.is_ascii_alphabetic())
                    || (prev.is_ascii_alphabetic() && current.is_ascii_digit())
            }
            None => false,
        };

        if insert_space && !result.ends_with(' ') {
            result.push(' ');
        }

        result.push(current);
        previous = Some(current);
    }

    result
}

fn species_name(species: &Species) -> String {
    humanize_identifier(format!("{:?}", species))
}

fn move_name(mov: &PokemonMove) -> String {
    humanize_identifier(format!("{:?}", mov))
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VolatileStatusState {
    /// Volatile with a turn counter.
    /// Zero means permanent. One means removal at the next end of turn.
    /// Substitute stores HP instead of a turn count.
    TurnStatus(VolatileStatus, u16),
    MoveStatus(VolatileStatus, u16),
    Charging(PokemonMove, Vec<crate::state::battle::FieldSlot>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PokemonGender {
    Male,
    Female,
    Genderless,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Nature {
    Hardy,
    Lonely,
    Adamant,
    Naughty,
    Brave,
    Bold,
    Docile,
    Impish,
    Lax,
    Relaxed,
    Modest,
    Mild,
    Bashful,
    Rash,
    Quiet,
    Calm,
    Gentle,
    Careful,
    Quirky,
    Sassy,
    Timid,
    Hasty,
    Jolly,
    Naive,
    Serious,
}

/// Excludes `used_moves_this_field` from equality and hashing.
/// This runtime history must not prevent branch coalescing.
#[derive(Clone)]
pub struct PokemonState {
    /// Stable party index.
    /// It survives switches and form changes.
    /// It also distinguishes duplicate species.
    pub mon_id: u8,
    pub fainted: bool,
    pub species: Species,
    pub types: Vec<PokemonType>,
    pub is_tera: bool,
    pub is_mega: bool,
    pub has_mega_form: bool,

    pub level: u8,
    pub hp: u16,
    pub moves: [Option<PokemonMove>; 4],
    pub move_pp: [u8; 4],
    pub max_pp: [u8; 4],
    pub item: Item,
    /// Last consumed item.
    /// Harvest and Recycle can restore it.
    /// Item removal without consumption does not set it.
    pub consumed_item: Option<Item>,
    /// Pending Cud Chew Berry and ready state.
    /// False waits through one end of turn. True activates at this end of turn.
    pub cud_chew_pending: Option<(Item, bool)>,
    /// True after this Pokémon loses its held item on the field.
    /// A new item or switch clears it.
    /// Unburden uses this value.
    pub item_lost: bool,

    // ── Per-turn event flags (cleared in end_turn Phase 5 and on switch-out) ────────
    /// True after any damage this turn.
    /// Assurance uses this value.
    pub damaged_this_turn: bool,
    /// Slots that damaged this Pokémon with direct move hits this turn.
    /// Avalanche uses this list.
    pub damaged_by_this_turn: Vec<crate::state::battle::FieldSlot>,
    /// Damage and source from the latest physical direct hit this turn.
    /// Counter uses these values.
    pub last_physical_damage_taken: u16,
    pub last_physical_attacker: Option<crate::state::battle::FieldSlot>,
    /// Damage and source from the latest special direct hit this turn.
    /// Mirror Coat uses these values.
    pub last_special_damage_taken: u16,
    pub last_special_attacker: Option<crate::state::battle::FieldSlot>,
    /// Damage and source from the latest direct hit this turn.
    /// Metal Burst and Comeuppance use these values.
    pub last_damage_taken: u16,
    pub last_damage_attacker: Option<crate::state::battle::FieldSlot>,
    /// True after a stat stage increases this turn.
    pub stats_raised_this_turn: bool,
    /// True after a stat stage decreases this turn.
    pub stats_lowered_this_turn: bool,
    /// True after a switch enters this Pokémon during the current turn.
    /// Battle leads do not set it.
    pub switched_in_this_turn: bool,
    /// Consecutive successful stalling moves.
    /// Controls the one-third success decay.
    pub stall_counter: u8,
    /// Consecutive successful Ally Switch uses.
    /// This decay is separate from `stall_counter`.
    pub ally_switch_counter: u8,
    pub nature: Nature,

    pub boosts: PokemonBoostTable,
    pub stats: PokemonStatsTable,

    pub status: Option<Status>,
    pub volatiles: Vec<VolatileStatusState>,

    pub ability: Ability,

    pub gender: PokemonGender,
    pub weight_hg: u16,

    pub tera_type: PokemonType,

    pub mega_species: Option<Species>,
    pub mega_ability: Option<Ability>,

    pub last_move_failed: bool, //For stomping tantrum

    /// True while Rest causes the current sleep.
    /// Rest uses a fixed duration instead of the normal random duration.
    pub rest_sleep: bool,

    pub original_ability: Option<Ability>,
    pub last_used_move: Option<PokemonMove>,
    /// Consecutive successful uses of the same move.
    /// The Metronome item uses this count.
    pub consecutive_move_count: u8,
    /// Move slots used since entry.
    /// Last Resort uses this array.
    pub used_moves_this_field: [bool; 4],

    /// True after a once-per-battle entry ability activates.
    /// This value persists across switches.
    pub one_time_ability_used: bool,

    /// True after this Pokémon eats a Berry.
    /// This value persists for the battle.
    /// Belch requires it.
    pub ate_berry_this_battle: bool,

    /// True during the first turn that this Pokémon can act.
    /// Fake Out and First Impression require it.
    pub first_move_on_field: bool,

    /// Preserves `first_move_on_field` after a mid-turn entry.
    /// The next turn is the Pokémon's first chance to act.
    pub first_turn_on_field_pending: bool,

    /// True on the turn that a switch action enters this Pokémon.
    /// Speed Boost skips this entry turn.
    /// Faint replacements do not set this value.
    pub entered_this_turn: bool,

    /// State before Imposter or Transform.
    /// The box permits the recursive type.
    pub pre_transform: Option<Box<PokemonState>>,

    /// Types before Mimicry.
    /// Terrain end or a switch restores them.
    pub pre_mimicry_types: Option<Vec<crate::state::dex_data::PokemonType>>,

    pub evs: [u8; 6],
    pub ivs: [u8; 6],

    /// Direct damaging hits taken since entry.
    /// Rage Fist uses this count.
    pub times_hit: u16,

    /// Active Illusion disguise species.
    /// Calculations still use the true species.
    pub illusion_disguise: Option<crate::data::species::Species>,
}

impl PartialEq for PokemonState {
    fn eq(&self, other: &Self) -> bool {
        self.mon_id == other.mon_id
            && self.fainted == other.fainted
            && self.species == other.species
            && self.types == other.types
            && self.is_tera == other.is_tera
            && self.is_mega == other.is_mega
            && self.has_mega_form == other.has_mega_form
            && self.level == other.level
            && self.hp == other.hp
            && self.moves == other.moves
            && self.move_pp == other.move_pp
            && self.max_pp == other.max_pp
            && self.item == other.item
            && self.consumed_item == other.consumed_item
            && self.cud_chew_pending == other.cud_chew_pending
            && self.item_lost == other.item_lost
            && self.damaged_this_turn == other.damaged_this_turn
            && self.damaged_by_this_turn == other.damaged_by_this_turn
            && self.last_physical_damage_taken == other.last_physical_damage_taken
            && self.last_physical_attacker == other.last_physical_attacker
            && self.last_special_damage_taken == other.last_special_damage_taken
            && self.last_special_attacker == other.last_special_attacker
            && self.last_damage_taken == other.last_damage_taken
            && self.last_damage_attacker == other.last_damage_attacker
            && self.stats_raised_this_turn == other.stats_raised_this_turn
            && self.stats_lowered_this_turn == other.stats_lowered_this_turn
            && self.switched_in_this_turn == other.switched_in_this_turn
            && self.stall_counter == other.stall_counter
            && self.nature == other.nature
            && self.boosts == other.boosts
            && self.stats == other.stats
            && self.status == other.status
            && self.volatiles == other.volatiles
            && self.ability == other.ability
            && self.gender == other.gender
            && self.weight_hg == other.weight_hg
            && self.tera_type == other.tera_type
            && self.mega_species == other.mega_species
            && self.mega_ability == other.mega_ability
            && self.last_move_failed == other.last_move_failed
            && self.original_ability == other.original_ability
            && self.last_used_move == other.last_used_move
            && self.consecutive_move_count == other.consecutive_move_count
            // Move-usage history affects Last Resort but is moot once fainted; skip the
            // comparison for fainted mons (self.fainted == other.fainted already holds above).
            && (self.fainted || self.used_moves_this_field == other.used_moves_this_field)
            && self.one_time_ability_used == other.one_time_ability_used
            && self.ate_berry_this_battle == other.ate_berry_this_battle
            && self.first_move_on_field == other.first_move_on_field
            && self.first_turn_on_field_pending == other.first_turn_on_field_pending
            && self.entered_this_turn == other.entered_this_turn
            && self.pre_transform == other.pre_transform
            && self.pre_mimicry_types == other.pre_mimicry_types
            && self.evs == other.evs
            && self.ivs == other.ivs
            && self.times_hit == other.times_hit
            && self.illusion_disguise == other.illusion_disguise
    }
}

impl Eq for PokemonState {}

impl std::hash::Hash for PokemonState {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.mon_id.hash(state);
        self.fainted.hash(state);
        self.species.hash(state);
        self.types.hash(state);
        self.is_tera.hash(state);
        self.is_mega.hash(state);
        self.has_mega_form.hash(state);
        self.level.hash(state);
        self.hp.hash(state);
        self.moves.hash(state);
        self.move_pp.hash(state);
        self.max_pp.hash(state);
        self.item.hash(state);
        self.consumed_item.hash(state);
        self.cud_chew_pending.hash(state);
        self.item_lost.hash(state);
        self.damaged_this_turn.hash(state);
        self.damaged_by_this_turn.hash(state);
        self.last_physical_damage_taken.hash(state);
        self.last_physical_attacker.hash(state);
        self.last_special_damage_taken.hash(state);
        self.last_special_attacker.hash(state);
        self.last_damage_taken.hash(state);
        self.last_damage_attacker.hash(state);
        self.stats_raised_this_turn.hash(state);
        self.stats_lowered_this_turn.hash(state);
        self.switched_in_this_turn.hash(state);
        self.stall_counter.hash(state);
        self.nature.hash(state);
        self.boosts.hash(state);
        self.stats.hash(state);
        self.status.hash(state);
        self.volatiles.hash(state);
        self.ability.hash(state);
        self.gender.hash(state);
        self.weight_hg.hash(state);
        self.tera_type.hash(state);
        self.mega_species.hash(state);
        self.mega_ability.hash(state);
        self.last_move_failed.hash(state);
        self.original_ability.hash(state);
        self.last_used_move.hash(state);
        // Only hash used_moves_this_field for non-fainted mons — must match PartialEq.
        if !self.fainted {
            self.used_moves_this_field.hash(state);
        }
        self.one_time_ability_used.hash(state);
        self.ate_berry_this_battle.hash(state);
        self.first_move_on_field.hash(state);
        self.first_turn_on_field_pending.hash(state);
        self.entered_this_turn.hash(state);
        self.pre_transform.hash(state);
        self.pre_mimicry_types.hash(state);
        self.evs.hash(state);
        self.ivs.hash(state);
        self.times_hit.hash(state);
        self.illusion_disguise.hash(state);
    }
}

impl std::fmt::Display for PokemonState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let stat_names = ["HP", "Atk", "Def", "SpA", "SpD", "Spe"];
        let stats_str = stat_names
            .iter()
            .enumerate()
            .map(|(i, name)| format!("{}: {}", name, self.stats[i]))
            .collect::<Vec<_>>()
            .join(", ");

        let boosts_str = {
            let boost_names = ["Atk", "Def", "SpA", "SpD", "Spe", "Acc", "Eva"];
            let active_boosts: Vec<String> = self
                .boosts
                .iter()
                .enumerate()
                .filter(|(_, b)| **b != 0)
                .map(|(i, b)| format!("{}{:+}", boost_names[i], b))
                .collect();
            if active_boosts.is_empty() {
                "none".to_string()
            } else {
                active_boosts.join(", ")
            }
        };

        let status_str = self
            .status
            .as_ref()
            .map(|s| format!("{:?}", s))
            .unwrap_or_else(|| "Healthy".to_string());
        let item_str = format!("{:?}", self.item);
        let ability_str = format!("{:?}", self.ability);
        let nature_str = format!("{:?}", self.nature);

        let evs_str = self
            .evs
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("/");
        let ivs_str = self
            .ivs
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("/");

        let vol_str = if self.volatiles.is_empty() {
            "none".to_string()
        } else {
            self.volatiles
                .iter()
                .map(|v| format!("{:?}", v))
                .collect::<Vec<_>>()
                .join(", ")
        };

        let tera_info = if self.is_tera {
            format!("Tera({:?})", self.tera_type)
        } else {
            "No Tera".to_string()
        };
        let mega_info = if self.has_mega_form {
            self.mega_species
                .as_ref()
                .map(|s| format!("Mega({:?})", s))
                .unwrap_or_else(|| "Has Mega (unknown species)".to_string())
        } else {
            "No Mega".to_string()
        };

        let moves_str = self
            .moves
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let name = m
                    .as_ref()
                    .map(move_name)
                    .unwrap_or_else(|| format!("Move {}", i + 1));
                let pp = self.move_pp.get(i).copied().unwrap_or(0);
                format!("{} (PP {})", name, pp)
            })
            .collect::<Vec<_>>()
            .join(", ");

        write!(
            f,
            "{} ({}/{} HP), Item: {}, Ability: {}, Nature: {}\n    Stats: {}\n    Boosts: {}\n    Status: {}\n    Volatiles: {}\n    {} | {}\n    Moves: {}\n    EVs: {}\n    IVs: {}",
            species_name(&self.species),
            self.hp,
            self.stats[0],
            item_str,
            ability_str,
            nature_str,
            stats_str,
            boosts_str,
            status_str,
            vol_str,
            tera_info,
            mega_info,
            moves_str,
            evs_str,
            ivs_str
        )
    }
}

impl std::fmt::Debug for PokemonState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

// --- Stat Calculation Helpers ---

pub fn normalize_string(name: impl AsRef<str>) -> String {
    name.as_ref()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn parse_nature_str(s: &str) -> Option<Nature> {
    match s.trim() {
        "Hardy" => Some(Nature::Hardy),
        "Lonely" => Some(Nature::Lonely),
        "Adamant" => Some(Nature::Adamant),
        "Naughty" => Some(Nature::Naughty),
        "Brave" => Some(Nature::Brave),
        "Bold" => Some(Nature::Bold),
        "Docile" => Some(Nature::Docile),
        "Impish" => Some(Nature::Impish),
        "Lax" => Some(Nature::Lax),
        "Relaxed" => Some(Nature::Relaxed),
        "Modest" => Some(Nature::Modest),
        "Mild" => Some(Nature::Mild),
        "Bashful" => Some(Nature::Bashful),
        "Rash" => Some(Nature::Rash),
        "Quiet" => Some(Nature::Quiet),
        "Calm" => Some(Nature::Calm),
        "Gentle" => Some(Nature::Gentle),
        "Careful" => Some(Nature::Careful),
        "Quirky" => Some(Nature::Quirky),
        "Sassy" => Some(Nature::Sassy),
        "Timid" => Some(Nature::Timid),
        "Hasty" => Some(Nature::Hasty),
        "Jolly" => Some(Nature::Jolly),
        "Naive" => Some(Nature::Naive),
        "Serious" => Some(Nature::Serious),
        _ => None,
    }
}

/// Returns stat multipliers as [atk, def, spa, spd, spe] for the given nature.
pub(crate) fn nature_stat_modifiers(nature: &Nature) -> [f32; 5] {
    match nature {
        Nature::Hardy | Nature::Docile | Nature::Bashful | Nature::Quirky | Nature::Serious => {
            [1.0, 1.0, 1.0, 1.0, 1.0]
        }
        // +Atk
        Nature::Lonely => [1.1, 0.9, 1.0, 1.0, 1.0],
        Nature::Adamant => [1.1, 1.0, 0.9, 1.0, 1.0],
        Nature::Naughty => [1.1, 1.0, 1.0, 0.9, 1.0],
        Nature::Brave => [1.1, 1.0, 1.0, 1.0, 0.9],
        // +Def
        Nature::Bold => [0.9, 1.1, 1.0, 1.0, 1.0],
        Nature::Impish => [1.0, 1.1, 0.9, 1.0, 1.0],
        Nature::Lax => [1.0, 1.1, 1.0, 0.9, 1.0],
        Nature::Relaxed => [1.0, 1.1, 1.0, 1.0, 0.9],
        // +SpA
        Nature::Modest => [0.9, 1.0, 1.1, 1.0, 1.0],
        Nature::Mild => [1.0, 0.9, 1.1, 1.0, 1.0],
        Nature::Rash => [1.0, 1.0, 1.1, 0.9, 1.0],
        Nature::Quiet => [1.0, 1.0, 1.1, 1.0, 0.9],
        // +SpD
        Nature::Calm => [0.9, 1.0, 1.0, 1.1, 1.0],
        Nature::Gentle => [1.0, 0.9, 1.0, 1.1, 1.0],
        Nature::Careful => [1.0, 1.0, 0.9, 1.1, 1.0],
        Nature::Sassy => [1.0, 1.0, 1.0, 1.1, 0.9],
        // +Spe
        Nature::Timid => [0.9, 1.0, 1.0, 1.0, 1.1],
        Nature::Hasty => [1.0, 0.9, 1.0, 1.0, 1.1],
        Nature::Jolly => [1.0, 1.0, 0.9, 1.0, 1.1],
        Nature::Naive => [1.0, 1.0, 1.0, 0.9, 1.1],
    }
}

pub(crate) fn calc_hp(base: u16, iv: u8, ev: u8, level: u8) -> u16 {
    let ev_contrib = ev as u16 / 4;
    (2 * base + iv as u16 + ev_contrib) * level as u16 / 100 + level as u16 + 10
}

pub(crate) fn calc_stat(base: u16, iv: u8, ev: u8, level: u8, nature_mod: f32) -> u16 {
    let ev_contrib = ev as u16 / 4;
    let inner = (2 * base + iv as u16 + ev_contrib) * level as u16 / 100 + 5;
    (inner as f32 * nature_mod).floor() as u16
}

/// Scale EVs from Showdown stat-points format (0–252) to internal (0–252).
///
/// Callers outside this module (the meta determinizer) need this to compare a
/// usage-data spread, which is authored in 0–32 points, against the inference
/// engine's EV bounds, which are already scaled. Re-deriving `8p − 4` at the
/// call site would be a silent divergence waiting to happen.
///
/// Note the `as u8`: a point value above 32 wraps rather than erroring
/// (`p = 33` yields `260`, which truncates to `4`). Callers must clamp first.
pub(crate) fn scale_evs_for_stat_points(mut evs: [u8; 6]) -> [u8; 6] {
    for ev in &mut evs {
        *ev = ((i16::from(*ev) * 8) - 4).max(0) as u8;
    }
    evs
}

/// Compute the in-game PP for a move (with PP Max applied).
fn compute_pp_for_move(move_name: &PokemonMove, move_dex: &HashMap<PokemonMove, MoveData>) -> u8 {
    if *move_name == PokemonMove::Protect {
        return 8;
    }
    let Some(data) = move_dex.get(move_name) else {
        return 0;
    };
    match data.pp {
        5 => 8,
        10 => 12,
        15 => 16,
        pp if pp > 15 => 20,
        pp => pp,
    }
}

/// Compute the full move PP array for a 4-move set.
fn compute_move_pp(
    moves: &[Option<PokemonMove>; 4],
    move_dex: &HashMap<PokemonMove, MoveData>,
) -> [u8; 4] {
    let mut pp = [0u8; 4];
    for (i, mov) in moves.iter().enumerate() {
        if let Some(m) = mov {
            pp[i] = compute_pp_for_move(m, move_dex);
        }
    }
    pp
}

/// Resolve mega-evolution species and ability from the Pokémon's held item.
fn resolve_mega_info(
    species: &Species,
    item: &Item,
    dex_entry: Option<&PokemonData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> (bool, Option<Species>, Option<Ability>) {
    let is_mega = dex_entry
        .map(|d| is_mega_dex_entry(species, d))
        .unwrap_or(false);
    if is_mega {
        return (true, None, None);
    }

    let item_str = if *item == Item::None {
        String::new()
    } else {
        normalize_string(format!("{:?}", item))
    };
    let mega_sp = resolve_mega_species(species, &item_str, pokemon_dex);
    let mega_ab = mega_sp
        .as_ref()
        .and_then(|key| pokemon_dex.get(key))
        .and_then(|d| d.primary_ability.clone());
    (false, mega_sp, mega_ab)
}

#[allow(clippy::too_many_arguments)]
pub fn build_pokemon_state(
    species: Species,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    level: Option<u8>,
    moves: Option<[Option<PokemonMove>; 4]>,
    gender: Option<PokemonGender>,
    ability: Option<Ability>,
    nature: Option<Nature>,
    item: Option<Item>,
    tera_type: Option<PokemonType>,
    evs: Option<[u8; 6]>,
    ivs: Option<[u8; 6]>,
    use_stat_points: bool,
) -> PokemonState {
    let dex_entry = pokemon_dex.get(&species);

    let level = level.unwrap_or(50);
    let moves = moves.unwrap_or([None, None, None, None]);
    let gender = gender.unwrap_or_else(|| {
        dex_entry
            .map(|d| d.default_gender)
            .unwrap_or(PokemonGender::Genderless)
    });
    let ability = ability.unwrap_or(Ability::Illuminate);
    let nature = nature.unwrap_or(Nature::Hardy);
    let item = item.unwrap_or(Item::None);
    let tera_type = tera_type.unwrap_or(PokemonType::Normal);
    let mut evs = evs.unwrap_or([0; 6]);
    let ivs = ivs.unwrap_or([31; 6]);

    if use_stat_points {
        evs = scale_evs_for_stat_points(evs);
    }

    let move_pp = compute_move_pp(&moves, move_dex);
    let max_pp = move_pp;
    let (types, base_stats, weight_hg) = dex_entry
        .map(|d| (d.types.clone(), d.base_stats, d.weight))
        .unwrap_or_else(|| (vec![PokemonType::Normal], [100u16; 6], 0u16));
    let stats = calc_stats_for_level(base_stats, ivs, evs, level, &nature);
    let (is_mega, mega_species, mega_ability) =
        resolve_mega_info(&species, &item, dex_entry, pokemon_dex);

    PokemonState {
        // Default identity; `parse_team_sheet` overwrites this with the party-order index.
        mon_id: 0,
        fainted: false,
        species,
        types,
        is_tera: false,
        is_mega,
        has_mega_form: mega_species.is_some(),
        level,
        hp: stats[0],
        moves,
        move_pp,
        max_pp,
        item,
        nature,
        boosts: [0; 7],
        stats,
        status: None,
        volatiles: Vec::new(),
        ability,
        gender,
        weight_hg,
        tera_type,
        mega_species,
        mega_ability,
        last_move_failed: false,
        rest_sleep: false,
        original_ability: None,
        last_used_move: None,
        consecutive_move_count: 0,
        used_moves_this_field: [false; 4],
        one_time_ability_used: false,
        ate_berry_this_battle: false,
        first_move_on_field: false,
        first_turn_on_field_pending: false,
        entered_this_turn: false,
        consumed_item: None,
        cud_chew_pending: None,
        item_lost: false,
        damaged_this_turn: false,
        damaged_by_this_turn: Vec::new(),
        last_physical_damage_taken: 0,
        last_physical_attacker: None,
        last_special_damage_taken: 0,
        last_special_attacker: None,
        last_damage_taken: 0,
        last_damage_attacker: None,
        stats_raised_this_turn: false,
        stats_lowered_this_turn: false,
        switched_in_this_turn: false,
        stall_counter: 0,
        ally_switch_counter: 0,
        pre_transform: None,
        pre_mimicry_types: None,
        evs,
        ivs,
        times_hit: 0,
        illusion_disguise: None,
    }
}

pub fn calc_stats_for_level(
    base_stats: [u16; 6],
    ivs: [u8; 6],
    evs: [u8; 6],
    level: u8,
    nature: &Nature,
) -> PokemonStatsTable {
    let hp = calc_hp(base_stats[0], ivs[0], evs[0], level);
    let mods = nature_stat_modifiers(nature);
    [
        hp,
        calc_stat(base_stats[1], ivs[1], evs[1], level, mods[0]),
        calc_stat(base_stats[2], ivs[2], evs[2], level, mods[1]),
        calc_stat(base_stats[3], ivs[3], evs[3], level, mods[2]),
        calc_stat(base_stats[4], ivs[4], evs[4], level, mods[3]),
        calc_stat(base_stats[5], ivs[5], evs[5], level, mods[4]),
    ]
}

/// `pub` because the server's `GET /api/dex/species` needs the same notion of
/// "this dex entry is a Mega forme" to keep Megas out of the teamsheet species
/// picker — a Mega is reached by holding the stone, never written on a sheet.
pub fn is_mega_dex_entry(species_key: &Species, data: &PokemonData) -> bool {
    let forme_is_mega = data
        .forme
        .as_ref()
        .map(|f| f.to_string().to_lowercase().contains("mega"))
        .unwrap_or(false);

    forme_is_mega || species_key.to_string().to_lowercase().contains("mega")
}

fn resolve_mega_species(
    base_species_key: &Species,
    item_key: &str,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> Option<Species> {
    if item_key.is_empty() {
        return None;
    }

    let mut fallback: Option<Species> = None;

    for (candidate_key, data) in pokemon_dex {
        if data.required_item.as_deref() != Some(item_key) {
            continue;
        }

        let matches_base_species = data.base_species.as_ref() == Some(base_species_key)
            || data.battle_only.as_ref() == Some(base_species_key);
        if !matches_base_species {
            continue;
        }

        if is_mega_dex_entry(candidate_key, data) {
            return Some(candidate_key.clone());
        }

        if fallback.is_none() {
            fallback = Some(candidate_key.clone());
        }
    }

    fallback
}

/// Enumerate every possible Mega Evolution form of `base_species_key`,
/// regardless of held item — the same back-pointer match `resolve_mega_species`
/// uses (a mega's own dex entry has `base_species`/`battle_only` pointing back
/// to its base), minus that function's `required_item` filter. `pub` (not
/// `pub(crate)`) because tracker mode calls this from the `server` binary
/// crate, not just within this lib crate. Used to auto-fill an unambiguous
/// `mega` line (exactly one form) or disambiguate a short suffix (`mega y`
/// for Charizard-Mega-Y) without needing to know which mega stone is held.
pub fn mega_forms_of(
    base_species_key: &Species,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> Vec<Species> {
    pokemon_dex
        .iter()
        .filter(|(candidate_key, data)| {
            let matches_base_species = data.base_species.as_ref() == Some(base_species_key)
                || data.battle_only.as_ref() == Some(base_species_key);
            matches_base_species && is_mega_dex_entry(candidate_key, data)
        })
        .map(|(candidate_key, _)| candidate_key.clone())
        .collect()
}

// --- Team Sheet Parsing ---

/// Parse a "stat_value stat_name" token (e.g. "252 SpA") into (index, value).
fn parse_stat_token(token: &str) -> Option<(usize, u8)> {
    let mut iter = token.trim().splitn(2, ' ');
    let val: u8 = iter.next()?.trim().parse().ok()?;
    let name = iter.next()?.trim();
    let idx = match name {
        "HP" => 0,
        "Atk" => 1,
        "Def" => 2,
        "SpA" => 3,
        "SpD" => 4,
        "Spe" => 5,
        _ => return None,
    };
    Some((idx, val))
}

/// Parse a "EVs:" or "IVs:" line into a stat array. `default_val` is the fill value (0 for EVs, 31 for IVs).
fn parse_stat_line(rest: &str, default_val: u8) -> [u8; 6] {
    let mut stats = [default_val; 6];
    for token in rest.split('/') {
        if let Some((idx, val)) = parse_stat_token(token) {
            stats[idx] = val;
        }
    }
    stats
}

struct PokemonHeader {
    species_key: Species,
    item_str: String,
    explicit_gender: Option<PokemonGender>,
}

/// Parse the first line of a Showdown teamsheet block.
fn parse_pokemon_header(
    header: &str,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> Option<PokemonHeader> {
    let (name_part, item_str) = if let Some(at_idx) = header.find(" @ ") {
        (&header[..at_idx], header[at_idx + 3..].trim().to_string())
    } else {
        (header, String::new())
    };

    let mut paren_groups: Vec<&str> = Vec::new();
    let mut search = name_part;
    while let (Some(op), Some(cp)) = (search.find('('), search.find(')')) {
        paren_groups.push(&search[op + 1..cp]);
        search = &search[cp + 1..];
    }

    let text_before = name_part
        .find('(')
        .map(|op| name_part[..op].trim())
        .unwrap_or(name_part.trim());
    let explicit_gender = match paren_groups.last() {
        Some(&"M") => Some(PokemonGender::Male),
        Some(&"F") => Some(PokemonGender::Female),
        _ => None,
    };
    let non_gender: Vec<&str> = paren_groups
        .iter()
        .filter(|&&g| g != "M" && g != "F")
        .copied()
        .collect();
    let species_name = non_gender.last().copied().unwrap_or(text_before);

    let key_str = {
        let k = normalize_string(species_name);
        if pokemon_dex.contains_key(&Species::from_str(&k)) {
            k
        } else {
            normalize_string(text_before)
        }
    };
    let mut species_key = Species::from_str(&key_str);
    // A teamsheet may name a Mega form directly (e.g. "Tyranitar-Mega"). Mega
    // Evolution is an in-battle action driven by the held stone, not a build-time
    // state, so reduce such a line to its base species. build_pokemon_state's
    // resolve_mega_info then re-derives mega_species/has_mega_form from the held
    // mega stone, exactly as a "Tyranitar @ Tyranitarite" sheet would.
    if let Some(data) = pokemon_dex.get(&species_key)
        && is_mega_dex_entry(&species_key, data)
        && let Some(base) = data.base_species.clone()
        && base != species_key
    {
        species_key = base;
    }
    if pokemon_dex.get(&species_key).is_none() {
        eprintln!(
            "Warning: '{}' not found in dex (key: '{:?}')",
            text_before, species_key
        );
    }
    Some(PokemonHeader {
        species_key,
        item_str,
        explicit_gender,
    })
}

/// Parses a Showdown-format teamsheet file and returns a Vec of PokemonStates.
/// Each Pokemon's stats are calculated from base stats, EVs, IVs, level, and nature.
pub fn parse_team_sheet(
    path: &str,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    use_stat_points: bool,
) -> Vec<PokemonState> {
    let content = fs::read_to_string(path).expect("Failed to read team sheet file");
    parse_team_sheet_str(&content, pokemon_dex, move_dex, use_stat_points)
}

/// Parses Showdown-format teamsheet text (as from a paste or an API request)
/// and returns a Vec of PokemonStates.
pub fn parse_team_sheet_str(
    content: &str,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    use_stat_points: bool,
) -> Vec<PokemonState> {
    let content = content.replace("\r\n", "\n");
    let mut team = Vec::new();

    for block in content.split("\n\n") {
        let lines: Vec<&str> = block
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        if lines.is_empty() {
            continue;
        }

        let Some(header) = parse_pokemon_header(lines[0], pokemon_dex) else {
            continue;
        };

        let mut ability: Option<Ability> = None;
        let mut level: Option<u8> = None;
        let mut tera_type: Option<PokemonType> = None;
        let mut evs: Option<[u8; 6]> = None;
        let mut ivs: Option<[u8; 6]> = None;
        let mut nature: Option<Nature> = None;
        let mut moves: [Option<PokemonMove>; 4] = [None, None, None, None];
        let mut move_count = 0;

        for &line in &lines[1..] {
            if let Some(rest) = line.strip_prefix("Ability:") {
                ability = Some(Ability::from_str(rest.trim()));
            } else if let Some(rest) = line.strip_prefix("Level:") {
                level = Some(rest.trim().parse().unwrap_or(50));
            } else if let Some(rest) = line.strip_prefix("Tera Type:") {
                tera_type = Some(parse_type(rest.trim()).unwrap_or(PokemonType::Normal));
            } else if let Some(rest) = line.strip_prefix("EVs:") {
                // Store the raw teamsheet points as-is — `build_pokemon_state` is the
                // single place that applies `scale_evs_for_stat_points` (gated on the
                // same `use_stat_points` flag threaded through to it below). Scaling
                // here too double-applied the transform: the second pass ran on an
                // already-scaled u8, overflowed, and silently wrapped mod 256,
                // producing a bogus final stat with no error (e.g. 20 points -> scaled
                // once to 156 -> scaled again to (156*8-4) mod 256 = 220).
                evs = Some(parse_stat_line(rest, 0));
            } else if let Some(rest) = line.strip_prefix("IVs:") {
                ivs = Some(parse_stat_line(rest, 31));
            } else if let Some(ns) = line.strip_suffix(" Nature") {
                nature = parse_nature_str(ns);
            } else if let Some(mv) = line.strip_prefix("- ")
                && move_count < 4
            {
                moves[move_count] = Some(PokemonMove::from_str(mv));
                move_count += 1;
            }
        }

        let item = if header.item_str.is_empty() {
            None
        } else {
            Some(Item::from_str(&header.item_str))
        };
        team.push(build_pokemon_state(
            header.species_key,
            pokemon_dex,
            move_dex,
            level,
            Some(moves),
            header.explicit_gender,
            ability,
            nature,
            item,
            tera_type,
            evs,
            ivs,
            use_stat_points,
        ));
    }

    // Assign each Pokémon a stable party-order id, unique within this team.
    for (idx, mon) in team.iter_mut().enumerate() {
        mon.mon_id = idx as u8;
    }

    team
}
