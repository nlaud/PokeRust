use dex_data::*

pub type PokemonStatsTable = [u16; 6]; // hp, atk, def, spa, spd, spe

pub enum VolatileStatusState{
    Status(VolatileStatus, u8),//Make this hold more information about each status, like turns remaining, etc.
}

#[Derive(Debug)]
pub enum PokemonGender{
    Male,
    Female,
    Genderless
}

#[Derive(Debug)]
pub struct PokemonState{
    pub mut fainted: bool,
    pub mut species: String,//should be all lower case, all non-alpha characters removed
    pub mut types: Vec<PokemonType>,
    pub tera_type: PokemonType,
    pub mut is_tera: bool,

    pub mut base_stats: PokemonStatsTable,
    pub mut boosts: PokemonBoostTable,

    pub level: u8,
    pub mut hp: u16,
    pub mut moves: [String; 4],
    pub mut item: String,

    pub mut status: Status,
    pub mut volatiles: Vec<VolatileStatusState>,

    pub base_ability: String,
    pub mut ability: String,

    pub last_move_failed: bool,//For stomping tantrum
    
    pub gender: GenderName,
    pub weight_hg: u16,
}

#[Derive(Debug)]
pub enum Player{
    P1,
    P2,
}

#[Derive(Debug)]
pub struct FieldSlot{
    pub player: Player,
    pub slot_index: u8,
}

#[Derive(Debug)]
pub struct MoveAction{
    pub move_name: String,
    pub priority: i8,
    pub mut user_slot: FieldSlot,
    pub target_slot: FieldSlot,
}

#[Derive(Debug)]
pub struct SwitchAction{
    pub speed: u16,
    pub user_slot: FieldSlot,
    pub switch_index: usize,
}

pub enum Action{
    MoveAction(MoveAction),
    SwitchAction(SwitchAction),
}

#[Derive(Debug)]
pub struct BattleState{
    pub active_per_side: u8,
    
    pub mod p1_active_mons: Vec<PokemonState>,
    pub mod p2_active_mons: Vec<PokemonState>,
    pub mod p1_back_mons: Vec<PokemonState>,
    pub mod p2_back_mons: Vec<PokemonState>,

    pub mod actionQueue: Vec<Action>,

    pub mod turnNumber: u16,

    pub mod turn_started:bool,
    pub mod turn_ended:bool,
}

pub struct TeamPreviewState{
    pub active_per_side: u8,
    pub p1_mons: Vec<PokemonState>,
    pub p2_mons: Vec<PokemonState>,
}

pub MatchState{
    BattleState(BattleState),
    TeamPreviewState(TeamPreviewState),
    GameOverState{winner:Player},
}