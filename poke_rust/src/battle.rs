use crate::pokemon::PokemonState;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::dex_data::{PokemonData, PseudoWeather, SideCondition, SelfSwitchType, SlotCondition, Terrain, Weather};
use std::collections::HashMap;

fn humanize_identifier(value: impl AsRef<str>) -> String {
    let value = value.as_ref();
    let mut result = String::new();
    let mut previous: Option<char> = None;

    for current in value.chars() {
        let insert_space = match previous {
            Some(prev) => (prev.is_ascii_lowercase() && current.is_ascii_uppercase())
                || (prev.is_ascii_digit() && current.is_ascii_alphabetic())
                || (prev.is_ascii_alphabetic() && current.is_ascii_digit()),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Player{
    P1,
    P2,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FieldSlot{
    pub player: Player,
    pub slot_index: u8,
}

impl std::fmt::Debug for FieldSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let p = match self.player { Player::P1 => "P1", Player::P2 => "P2" };
        write!(f, "{}_{}", p, self.slot_index)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MoveAction{
    pub move_name: PokemonMove,
    pub priority: i8,
    pub user_slot: FieldSlot,
    pub target_slot: Option<FieldSlot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SwitchAction{
    pub user_slot: FieldSlot,
    pub switch_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MegaAction{
    pub user_slot: FieldSlot,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TeraAction{
    pub user_slot: FieldSlot,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Action{
    MoveAction(MoveAction),
    SwitchAction(SwitchAction),
    MegaAction(MegaAction),
    TeraAction(TeraAction),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BattleState{
    pub active_per_side: u8,
    
    pub p1_active_mons: Vec<PokemonState>,
    pub p2_active_mons: Vec<PokemonState>,
    pub p1_back_mons: Vec<PokemonState>,
    pub p2_back_mons: Vec<PokemonState>,

    pub action_queue: Vec<Action>,

    pub turn_number: u16,

    //Both false = waiting for moves from both players
    //Started true, ended false = processing actions from action_queue
    //Both true = check if players have active fainted mons to send out
    pub turn_started: bool,
    pub turn_ended: bool,

    pub p1_has_tera: bool,
    pub p2_has_tera: bool,

    pub p1_has_mega: bool,
    pub p2_has_mega: bool,

    pub weather: Option<Weather>,
    pub weather_turns: Option<u8>,
    pub pseudo_weathers: Vec<PseudoWeather>,
    pub pseudo_weather_turns: Vec<u8>,
    pub terrain: Option<Terrain>,
    pub terrain_turns: Option<u8>,
    pub p1_side_conditions: Vec<SideCondition>,
    pub p1_side_condition_turns: Vec<u8>,
    pub p2_side_conditions: Vec<SideCondition>,
    pub p2_side_condition_turns: Vec<u8>,
    pub p1_slot_conditions: Vec<Vec<SlotCondition>>,
    pub p2_slot_conditions: Vec<Vec<SlotCondition>>,

    /// Set mid-turn after a self-switch move (U-turn, Baton Pass, etc.) fully resolves and the
    /// user is alive with a healthy bench.  While this is `Some`, `simulate_turn` returns to the
    /// caller so the player can choose a replacement; only the pending slot may switch, every
    /// other active slot must Pass.  Cleared once the replacement is sent in.
    pub self_switch_pending: Option<(FieldSlot, SelfSwitchType)>,
}

/// Format a single Pokémon's state as a multi-line string for display.
fn format_mon(m: &PokemonState) -> String {
    let stat_names = ["HP", "Atk", "Def", "SpA", "SpD", "Spe"];
    let stats_str = stat_names.iter().enumerate()
        .map(|(i, name)| format!("{}: {}", name, m.stats[i]))
        .collect::<Vec<_>>().join(", ");

    let boost_names = ["Atk", "Def", "SpA", "SpD", "Spe", "Acc", "Eva"];
    let active_boosts: Vec<String> = m.boosts.iter().enumerate()
        .filter(|(_, b)| **b != 0)
        .map(|(i, b)| format!("{}{:+}", boost_names[i], b))
        .collect();
    let boosts_str = if active_boosts.is_empty() { "none".to_string() } else { active_boosts.join(", ") };

    let status_str = m.status.as_ref().map(|s| format!("{:?}", s)).unwrap_or_else(|| "Healthy".to_string());
    let vol_str = if m.volatiles.is_empty() { "none".to_string() }
                  else { m.volatiles.iter().map(|v| format!("{:?}", v)).collect::<Vec<_>>().join(", ") };
    let tera_info = if m.is_tera { format!("Tera({:?})", m.tera_type) } else { "No Tera".to_string() };
    let mega_info = if m.has_mega_form {
        m.mega_species.as_ref().map(|s| format!("Mega({:?})", s)).unwrap_or_else(|| "Has Mega (unknown species)".to_string())
    } else { "No Mega".to_string() };
    let moves_str = m.moves.iter().enumerate()
        .map(|(i, mov)| {
            let name = mov.as_ref().map(|mv| humanize_identifier(format!("{:?}", mv))).unwrap_or_else(|| format!("Move {}", i + 1));
            format!("{} (PP {})", name, m.move_pp.get(i).copied().unwrap_or(0))
        })
        .collect::<Vec<_>>().join(", ");

    format!(
        "{} ({}/{} HP), Status: {}{}\n    Stats: {}\n    Boosts: {}\n    Volatiles: {}\n    {} | {}\n    Moves: {}",
        species_name(&m.species), m.hp, m.stats[0], status_str,
        if m.item != crate::data::item::Item::None {
            format!(", Item: {:?}, Ability: {:?}", m.item, m.ability)
        } else {
            format!(", Ability: {:?}", m.ability)
        },
        stats_str, boosts_str, vol_str, tera_info, mega_info, moves_str,
    )
}

/// Write a labelled team section (active or back) to `f`.
fn write_team_section(f: &mut std::fmt::Formatter<'_>, label: &str, mons: &[PokemonState]) -> std::fmt::Result {
    writeln!(f, "{}:", label)?;
    if mons.is_empty() {
        writeln!(f, "  (none)")
    } else {
        writeln!(f, "  {}", mons.iter().map(format_mon).collect::<Vec<_>>().join("\n  "))
    }
}

impl std::fmt::Display for BattleState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Turn {} (Started: {}, Ended: {})", self.turn_number, self.turn_started, self.turn_ended)?;

        write_team_section(f, "P1 Active", &self.p1_active_mons)?;
        write_team_section(f, "P1 Back", &self.p1_back_mons)?;
        writeln!(f, "P1 Has Tera: {} | Has Mega: {}", self.p1_has_tera, self.p1_has_mega)?;
        write_team_section(f, "P2 Active", &self.p2_active_mons)?;
        write_team_section(f, "P2 Back", &self.p2_back_mons)?;
        writeln!(f, "P2 Has Tera: {} | Has Mega: {}", self.p2_has_tera, self.p2_has_mega)?;

        if let Some(weather) = &self.weather {
            if let Some(turns) = self.weather_turns {
                writeln!(f, "Weather: {:?} ({}t)", weather, turns)?;
            } else {
                writeln!(f, "Weather: {:?}", weather)?;
            }
        }

        if !self.pseudo_weathers.is_empty() {
            let pseudo_strs: Vec<String> = self
                .pseudo_weathers
                .iter()
                .zip(self.pseudo_weather_turns.iter())
                .map(|(pw, turns)| format!("{:?} ({}t)", pw, turns))
                .collect();
            writeln!(f, "Pseudo-Weather: {}", pseudo_strs.join(", "))?;
        }

        if let Some(terrain) = &self.terrain {
            if let Some(turns) = self.terrain_turns {
                writeln!(f, "Terrain: {:?} ({}t)", terrain, turns)?;
            } else {
                writeln!(f, "Terrain: {:?}", terrain)?;
            }
        }

        if !self.p1_side_conditions.is_empty() {
            let p1_side_strs: Vec<String> = self
                .p1_side_conditions
                .iter()
                .zip(self.p1_side_condition_turns.iter())
                .map(|(sc, turns)| format!("{:?} ({}t)", sc, turns))
                .collect();
            writeln!(f, "P1 Side Conditions: {}", p1_side_strs.join(", "))?;
        }

        if !self.p2_side_conditions.is_empty() {
            let p2_side_strs: Vec<String> = self
                .p2_side_conditions
                .iter()
                .zip(self.p2_side_condition_turns.iter())
                .map(|(sc, turns)| format!("{:?} ({}t)", sc, turns))
                .collect();
            writeln!(f, "P2 Side Conditions: {}", p2_side_strs.join(", "))?;
        }

        let p1_has_slot_conds = self.p1_slot_conditions.iter().any(|slot_conds| !slot_conds.is_empty());
        if p1_has_slot_conds {
            for (slot_idx, slot_conds) in self.p1_slot_conditions.iter().enumerate() {
                if !slot_conds.is_empty() {
                    let slot_strs: Vec<String> = slot_conds.iter().map(|sc| format!("{:?}", sc)).collect();
                    writeln!(f, "  P1 Slot {}: {}", slot_idx, slot_strs.join(", "))?;
                }
            }
        }

        let p2_has_slot_conds = self.p2_slot_conditions.iter().any(|slot_conds| !slot_conds.is_empty());
        if p2_has_slot_conds {
            for (slot_idx, slot_conds) in self.p2_slot_conditions.iter().enumerate() {
                if !slot_conds.is_empty() {
                    let slot_strs: Vec<String> = slot_conds.iter().map(|sc| format!("{:?}", sc)).collect();
                    writeln!(f, "  P2 Slot {}: {}", slot_idx, slot_strs.join(", "))?;
                }
            }
        }

        if !self.action_queue.is_empty() {
            writeln!(f, "Action Queue:")?;
            for (i, action) in self.action_queue.iter().enumerate() {
                writeln!(f, "  {}: {:?}", i, action)?;
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TeamPreviewState{
    pub active_per_side: u8,
    pub brought_per_side: u8,
    pub p1_mons: Vec<PokemonState>,
    pub p2_mons: Vec<PokemonState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MatchState{
    BattleState(BattleState),
    TeamPreviewState(TeamPreviewState),
    GameOverState{winner:Player},
}

impl std::fmt::Display for MatchState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MatchState::BattleState(bs) => write!(f, "{}", bs),
            MatchState::TeamPreviewState(tp) => {
                write!(f, "TeamPreview: P1={} mons, P2={} mons", tp.p1_mons.len(), tp.p2_mons.len())
            }
            MatchState::GameOverState { winner } => {
                let w = match winner { Player::P1 => "P1", Player::P2 => "P2" };
                write!(f, "GameOver: winner={}", w)
            }
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct AttackCommand {
    pub move_slot: usize,
    pub target: Option<FieldSlot>,
    pub terastallize: bool,
    pub mega_evolve: bool,
}

impl std::fmt::Debug for AttackCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Atk({}", self.move_slot)?;
        if let Some(t) = &self.target {
            write!(f, "->{:?}", t)?;
        }
        if self.terastallize { write!(f, " TERA")?; }
        if self.mega_evolve { write!(f, " MEGA")?; }
        write!(f, ")")
    }
}

#[derive(Clone, PartialEq)]
pub struct SwitchCommand {
    pub party_index: usize,
}

impl std::fmt::Debug for SwitchCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sw({})", self.party_index)
    }
}

#[derive(Clone, PartialEq)]
pub enum BattleCommand {
    Attack(AttackCommand),
    Switch(SwitchCommand),
    Pass,
}

impl std::fmt::Debug for BattleCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BattleCommand::Attack(a) => write!(f, "{:?}", a),
            BattleCommand::Switch(s) => write!(f, "{:?}", s),
            BattleCommand::Pass => write!(f, "Pass"),
        }
    }
}

#[derive(Clone)]
pub struct TeamPreviewCommand {
    pub active_indices: Vec<usize>,
    pub back_indices: Vec<usize>,
}

impl std::fmt::Debug for TeamPreviewCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Preview({:?} | {:?})", self.active_indices, self.back_indices)
    }
}

#[derive(Clone)]
pub enum PlayerCommand {
    Battle(Vec<BattleCommand>),
    Pass,
    TeamPreview(TeamPreviewCommand),
    Forfeit,
}

impl std::fmt::Debug for PlayerCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlayerCommand::Battle(cmds) => {
                write!(f, "Battle[")?;
                for (i, cmd) in cmds.iter().enumerate() {
                    if i > 0 { write!(f, ", ")?; }
                    write!(f, "{:?}", cmd)?;
                }
                write!(f, "]")
            },
            PlayerCommand::TeamPreview(cmd) => write!(f, "{:?}", cmd),
            PlayerCommand::Forfeit => write!(f, "Forfeit"),
                PlayerCommand::Pass => write!(f, "Pass"),
        }
    }
}

/// Applies Mega Evolution to a Pokemon if it is eligible.
/// Returns true if Mega Evolution was applied.
pub fn try_mega_evolution(mon: &mut PokemonState, pokemon_dex: &HashMap<Species, PokemonData>) -> bool {
    if mon.fainted || mon.is_mega || !mon.has_mega_form {
        return false;
    }

    let mega_species_key = match mon.mega_species.clone() {
        Some(key) => key,
        None => return false,
    };

    let mega_data = match pokemon_dex.get(&mega_species_key) {
        Some(data) => data,
        None => return false,
    };

    let old_max_hp = mon.stats[0].max(1);
    let hp_ratio = mon.hp.min(old_max_hp) as f32 / old_max_hp as f32;
    let stats = crate::pokemon::calc_stats_for_level(mega_data.base_stats, mon.ivs, mon.evs, mon.level, &mon.nature);
    let new_max_hp = stats[0].max(1);
    let scaled_hp = (hp_ratio * new_max_hp as f32).floor() as u16;
    let hp = if mon.hp == 0 { 0 } else { scaled_hp.clamp(1, new_max_hp) };

    mon.species = mega_species_key;
    mon.types = mega_data.types.clone();
    mon.stats = stats;
    mon.hp = hp;
    mon.weight_hg = mega_data.weight;
    if let Some(ability) = mega_data.primary_ability.as_ref() {
        mon.ability = ability.clone();
    mon.base_ability = ability.clone();
    }
    mon.is_mega = true;
    mon.has_mega_form = false;
    mon.mega_species = None;

    true
}
