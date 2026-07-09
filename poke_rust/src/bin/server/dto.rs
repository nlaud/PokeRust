//! JSON DTOs for the web frontend. The engine's state types stay untouched
//! (several payload enums are code-generated); these structs are the wire format,
//! hiding engine bookkeeping fields and emitting display-name strings.

use serde::{Deserialize, Serialize};

// ── Shared views ─────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FieldSlotDto {
    pub player: PlayerDto,
    pub slot_index: u8,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PlayerDto {
    P1,
    P2,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct HpDto {
    pub current: u16,
    pub max: u16,
}

/// HP as observed in the event stream: exact for the observer's own side,
/// percent for the opponent's.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ObservedHpDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exact: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent: Option<u8>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct StatusDto {
    /// Short badge code: BRN, PSN, TOX, PAR, SLP, FRZ.
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turns: Option<u8>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct VolatileDto {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turns: Option<u16>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct NamedTurnsDto {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turns: Option<u8>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MoveViewDto {
    pub name: String,
    pub pp: u8,
    pub max_pp: u8,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PokemonView {
    pub mon_id: u8,
    /// Showdown display name, e.g. "Abomasnow-Mega" — also the sprite-slug source.
    pub species: String,
    pub level: u8,
    pub gender: String,
    pub types: Vec<String>,
    pub hp: HpDto,
    pub fainted: bool,
    pub status: Option<StatusDto>,
    pub volatiles: Vec<VolatileDto>,
    /// HP, Atk, Def, SpA, SpD, Spe.
    pub stats: [u16; 6],
    /// Atk, Def, SpA, SpD, Spe, Acc, Eva stages.
    pub boosts: [i8; 7],
    pub nature: String,
    /// HP, Atk, Def, SpA, SpD, Spe EVs.
    pub evs: [u8; 6],
    pub item: Option<String>,
    pub ability: String,
    pub moves: Vec<Option<MoveViewDto>>,
    pub is_tera: bool,
    pub tera_type: String,
    pub is_mega: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SideView {
    pub active: Vec<PokemonView>,
    pub back: Vec<PokemonView>,
    pub can_tera: bool,
    pub can_mega: bool,
    pub side_conditions: Vec<NamedTurnsDto>,
    pub slot_conditions: Vec<Vec<String>>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FieldView {
    pub weather: Option<NamedTurnsDto>,
    pub terrain: Option<NamedTurnsDto>,
    pub pseudo_weathers: Vec<NamedTurnsDto>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PreviewView {
    pub active_per_side: u8,
    pub brought_per_side: u8,
    pub p1_mons: Vec<PokemonView>,
    pub p2_mons: Vec<PokemonView>,
}

#[derive(Serialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum PhaseDto {
    TeamPreview,
    Normal,
    SelfSwitch,
    Replacement,
    GameOver,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BattleView {
    pub phase: PhaseDto,
    pub turn_number: u16,
    pub active_per_side: u8,
    pub brought_per_side: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<PreviewView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p1: Option<SideView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p2: Option<SideView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<FieldView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_switch: Option<FieldSlotDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub winner: Option<PlayerDto>,
}

// ── Legal commands ───────────────────────────────────────────────────────────

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CommandOptionDto {
    pub command: BattleCommandDto,
    /// Human-readable label, e.g. "Use Heat Wave -> P2's Sylveon [Tera]".
    pub description: String,
    /// Display name of the move (attack/struggle) or incoming Pokémon (switch).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SlotCommandsDto {
    pub slot_index: usize,
    /// True when the slot has no real choice (auto-Pass in self-switch/replacement phases).
    pub forced: bool,
    pub options: Vec<CommandOptionDto>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LegalCommandsView {
    pub phase: PhaseDto,
    pub slots: Vec<SlotCommandsDto>,
}

// ── Inbound commands ─────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum BattleCommandDto {
    #[serde(rename_all = "camelCase")]
    Attack {
        move_slot: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<FieldSlotDto>,
        #[serde(default)]
        terastallize: bool,
        #[serde(default)]
        mega_evolve: bool,
    },
    #[serde(rename_all = "camelCase")]
    Switch { party_index: usize },
    #[serde(rename_all = "camelCase")]
    Struggle {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<FieldSlotDto>,
    },
    Pass,
}

#[derive(Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PlayerCommandDto {
    #[serde(rename_all = "camelCase")]
    Battle { commands: Vec<BattleCommandDto> },
    Pass,
    #[serde(rename_all = "camelCase")]
    TeamPreview {
        active_indices: Vec<usize>,
        back_indices: Vec<usize>,
    },
}

// ── Event tree ───────────────────────────────────────────────────────────────

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EventNode {
    #[serde(flatten)]
    pub kind: EventKindDto,
    pub reactions: Vec<EventNode>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SwitchDto {
    pub slot: FieldSlotDto,
    pub species: String,
    pub level: u8,
    pub hp: ObservedHpDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<StatusDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tera_type: Option<String>,
}

/// One variant per engine `EventKind`; the `type` tag drives the frontend log renderer.
#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EventKindDto {
    #[serde(rename_all = "camelCase")]
    MoveUsed {
        user: FieldSlotDto,
        r#move: String,
        targets: Vec<FieldSlotDto>,
    },
    Switch {
        switch: SwitchDto,
    },
    #[serde(rename_all = "camelCase")]
    SimultaneousSwitch { switches: Vec<SwitchDto> },
    EndOfTurn,
    Faint {
        slot: FieldSlotDto,
    },
    #[serde(rename_all = "camelCase")]
    MegaEvolution { slot: FieldSlotDto, into: String },
    #[serde(rename_all = "camelCase")]
    Terastallization {
        slot: FieldSlotDto,
        tera_type: String,
    },
    #[serde(rename_all = "camelCase")]
    FormeChange {
        slot: FieldSlotDto,
        into: String,
        permanent: bool,
    },
    #[serde(rename_all = "camelCase")]
    TypeChanged {
        slot: FieldSlotDto,
        new_types: Vec<String>,
    },
    Cant {
        slot: FieldSlotDto,
        reason: String,
    },
    #[serde(rename_all = "camelCase")]
    ChargingMove { user: FieldSlotDto, r#move: String },
    #[serde(rename_all = "camelCase")]
    MustRecharge { slot: FieldSlotDto },
    #[serde(rename_all = "camelCase")]
    SingleMoveOrTurn { slot: FieldSlotDto, r#move: String },
    #[serde(rename_all = "camelCase")]
    DamageDealt {
        target: FieldSlotDto,
        new_hp: ObservedHpDto,
    },
    #[serde(rename_all = "camelCase")]
    Healed {
        target: FieldSlotDto,
        new_hp: ObservedHpDto,
    },
    #[serde(rename_all = "camelCase")]
    SetHp {
        target: FieldSlotDto,
        new_hp: ObservedHpDto,
    },
    Crit {
        target: FieldSlotDto,
    },
    Immune {
        target: FieldSlotDto,
    },
    Missed {
        target: FieldSlotDto,
    },
    #[serde(rename_all = "camelCase")]
    MoveFailed { slot: FieldSlotDto },
    Blocked {
        target: FieldSlotDto,
    },
    #[serde(rename_all = "camelCase")]
    HitCount { target: FieldSlotDto, hits: u8 },
    #[serde(rename_all = "camelCase")]
    StatusInflicted {
        target: FieldSlotDto,
        status: StatusDto,
    },
    #[serde(rename_all = "camelCase")]
    StatusCured {
        target: FieldSlotDto,
        status: StatusDto,
    },
    #[serde(rename_all = "camelCase")]
    TeamStatusCured { side: PlayerDto },
    #[serde(rename_all = "camelCase")]
    BoostChanged {
        target: FieldSlotDto,
        stat: String,
        stages: i8,
    },
    #[serde(rename_all = "camelCase")]
    BoostsCleared { target: FieldSlotDto },
    #[serde(rename_all = "camelCase")]
    BoostsInverted { target: FieldSlotDto },
    #[serde(rename_all = "camelCase")]
    BoostsSwapped {
        source: FieldSlotDto,
        target: FieldSlotDto,
    },
    #[serde(rename_all = "camelCase")]
    BoostsCopied {
        source: FieldSlotDto,
        target: FieldSlotDto,
    },
    #[serde(rename_all = "camelCase")]
    WeatherChanged { weather: Option<String> },
    #[serde(rename_all = "camelCase")]
    TerrainChanged { terrain: Option<String> },
    #[serde(rename_all = "camelCase")]
    PseudoWeatherStart { effect: String },
    #[serde(rename_all = "camelCase")]
    PseudoWeatherEnd { effect: String },
    #[serde(rename_all = "camelCase")]
    SideConditionStart { side: PlayerDto, condition: String },
    #[serde(rename_all = "camelCase")]
    SideConditionEnd { side: PlayerDto, condition: String },
    #[serde(rename_all = "camelCase")]
    SlotConditionStart {
        slot: FieldSlotDto,
        condition: String,
    },
    #[serde(rename_all = "camelCase")]
    SlotConditionEnd {
        slot: FieldSlotDto,
        condition: String,
    },
    #[serde(rename_all = "camelCase")]
    VolatileStart {
        target: FieldSlotDto,
        volatile: String,
    },
    #[serde(rename_all = "camelCase")]
    VolatileEnd {
        target: FieldSlotDto,
        volatile: String,
    },
    #[serde(rename_all = "camelCase")]
    PerishCount {
        target: FieldSlotDto,
        turns_left: u8,
    },
    #[serde(rename_all = "camelCase")]
    ItemRevealed { slot: FieldSlotDto, item: String },
    #[serde(rename_all = "camelCase")]
    ItemGained { slot: FieldSlotDto, item: String },
    #[serde(rename_all = "camelCase")]
    ItemLost {
        slot: FieldSlotDto,
        item: String,
        consumed: bool,
    },
    #[serde(rename_all = "camelCase")]
    AbilityRevealed { slot: FieldSlotDto, ability: String },
    #[serde(rename_all = "camelCase")]
    AnticipationShudder { slot: FieldSlotDto },
    #[serde(rename_all = "camelCase")]
    IllusionEnded {
        slot: FieldSlotDto,
        actual_species: String,
    },
    #[serde(rename_all = "camelCase")]
    Transformed {
        slot: FieldSlotDto,
        into_slot: FieldSlotDto,
        into_species: String,
    },
}

// ── Log / API request-response shells ────────────────────────────────────────

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TurnLogEntry {
    /// "Team Preview" or "Turn N"; the frontend renders a divider when the label changes.
    pub label: String,
    pub events: Vec<EventNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBattleRequest {
    pub p1_team: String,
    pub p2_team: String,
    pub active_per_side: u8,
    pub brought_per_side: u8,
    #[serde(default = "default_true")]
    pub stat_points: bool,
    #[serde(default = "default_true")]
    pub consider_crit: bool,
    #[serde(default = "default_damage_rolls")]
    pub damage_rolls: u8,
}

fn default_true() -> bool {
    true
}

fn default_damage_rolls() -> u8 {
    16
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBattleResponse {
    pub battle_id: String,
    pub state: BattleView,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetBattleResponse {
    pub state: BattleView,
    pub log: Vec<TurnLogEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnRequest {
    pub p1: PlayerCommandDto,
    pub p2: PlayerCommandDto,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnResponse {
    pub state: BattleView,
    pub events: Vec<EventNode>,
    pub probability: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub message: String,
}
