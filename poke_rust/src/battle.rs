use crate::pokemon::PokemonState;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::dex_data::PokemonData;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Player{
    P1,
    P2,
}

#[derive(Clone, Copy, PartialEq)]
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

#[derive(Debug, Clone)]
pub struct MoveAction{
    pub move_name: PokemonMove,
    pub priority: i8,
    pub user_slot: FieldSlot,
    pub target_slot: FieldSlot,
}

#[derive(Debug, Clone)]
pub struct SwitchAction{
    pub speed: u16,
    pub user_slot: FieldSlot,
    pub switch_index: usize,
}

#[derive(Debug, Clone)]
pub struct MegaAction{
    pub speed: u16,
    pub user_slot: FieldSlot,
}

#[derive(Debug, Clone)]
pub struct TeraAction{
    pub speed: u16,
    pub user_slot: FieldSlot,
}

#[derive(Debug, Clone)]
pub enum Action{
    MoveAction(MoveAction),
    SwitchAction(SwitchAction),
    MegaAction(MegaAction),
    TeraAction(TeraAction)
}

#[derive(Debug, Clone)]
pub struct BattleState{
    pub active_per_side: u8,
    
    pub p1_active_mons: Vec<PokemonState>,
    pub p2_active_mons: Vec<PokemonState>,
    pub p1_back_mons: Vec<PokemonState>,
    pub p2_back_mons: Vec<PokemonState>,

    pub action_queue: Vec<Action>,

    pub turn_number: u16,

    pub turn_started: bool,
    pub turn_ended: bool,

    pub p1_has_tera: bool,
    pub p2_has_tera: bool,

    pub p1_has_mega: bool,
    pub p2_has_mega: bool,
}

impl std::fmt::Display for BattleState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Turn {} (Started: {}, Ended: {})", self.turn_number, self.turn_started, self.turn_ended)?;
        
        let p1_active_names: Vec<String> = self.p1_active_mons.iter().map(|m| format!("{:?} ({} HP)", m.species, m.hp)).collect();
        let p1_back_names: Vec<String> = self.p1_back_mons.iter().map(|m| format!("{:?} ({} HP)", m.species, m.hp)).collect();
        
        let p2_active_names: Vec<String> = self.p2_active_mons.iter().map(|m| format!("{:?} ({} HP)", m.species, m.hp)).collect();
        let p2_back_names: Vec<String> = self.p2_back_mons.iter().map(|m| format!("{:?} ({} HP)", m.species, m.hp)).collect();

        writeln!(f, "P1 Active: [{}] | Back: [{}] | Tera: {} | Mega: {}", p1_active_names.join(", "), p1_back_names.join(", "), self.p1_has_tera, self.p1_has_mega)?;
        writeln!(f, "P2 Active: [{}] | Back: [{}] | Tera: {} | Mega: {}", p2_active_names.join(", "), p2_back_names.join(", "), self.p2_has_tera, self.p2_has_mega)?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct TeamPreviewState{
    pub active_per_side: u8,
    pub brought_per_side: u8,
    pub p1_mons: Vec<PokemonState>,
    pub p2_mons: Vec<PokemonState>,
}

#[derive(Debug, Clone)]
pub enum MatchState{
    BattleState(BattleState),
    TeamPreviewState(TeamPreviewState),
    GameOverState{winner:Player},
}

#[derive(Clone)]
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

#[derive(Clone)]
pub struct SwitchCommand {
    pub party_index: usize,
}

impl std::fmt::Debug for SwitchCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sw({})", self.party_index)
    }
}

#[derive(Clone)]
pub enum BattleCommand {
    Attack(AttackCommand),
    Switch(SwitchCommand),
}

impl std::fmt::Debug for BattleCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BattleCommand::Attack(a) => write!(f, "{:?}", a),
            BattleCommand::Switch(s) => write!(f, "{:?}", s),
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
