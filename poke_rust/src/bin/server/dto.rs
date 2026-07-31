//! Defines the JSON data objects for the web frontend.
//! Engine state types do not use serialization.
//! These objects hide engine fields and use display names.

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

/// Stores exact ally HP or an opponent HP percentage.
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

/// Stores an effect name and its remaining duration.
/// `turns` is the lower or exact value.
/// `turns_max` is a different upper value when necessary.
/// The range must include all values in the belief.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct NamedTurnsDto {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turns: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turns_max: Option<u8>,
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
    /// Showdown display name and sprite source.
    pub species: String,
    pub level: u8,
    pub gender: String,
    pub types: Vec<String>,
    pub hp: ObservedHpDto,
    pub fainted: bool,
    pub status: Option<StatusDto>,
    pub volatiles: Vec<VolatileDto>,
    /// HP, Atk, Def, SpA, SpD, and Spe.
    /// A masked view stores the lower bounds.
    pub stats: [u16; 6],
    /// Upper stat bounds.
    /// Equal to `stats` when each stat is exact.
    pub stats_max: [u16; 6],
    /// Atk, Def, SpA, SpD, Spe, Acc, Eva stages.
    pub boosts: [i8; 7],
    pub nature: String,
    /// Lower HP, Atk, Def, SpA, SpD, and Spe EV bounds.
    pub evs: [u8; 6],
    /// Upper EV bounds.
    pub evs_max: [u8; 6],
    pub item: Option<String>,
    pub ability: String,
    pub moves: Vec<Option<MoveViewDto>>,
    pub is_tera: bool,
    pub tera_type: String,
    pub is_mega: bool,
    /// True while the observer has multiple Illusion species candidates.
    pub is_illusion_suspected: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SideView {
    pub active: Vec<PokemonView>,
    pub back: Vec<PokemonView>,
    /// Preview species that did not enter the selected team.
    /// The frontend shows these species in gray.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub possible_back: Vec<PokemonView>,
    /// Replaced fainted opponents and their revealed data.
    /// Perfect information keeps them in the normal team lists.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fainted: Vec<PokemonView>,
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

/// Stores CNF predicates as readable OR clauses.
/// Perfect information and team preview omit this value.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BeliefView {
    pub clauses: Vec<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub belief: Option<BeliefView>,
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
    /// True when the slot must pass.
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
    Switch {
        party_index: usize,
    },
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
    Battle {
        commands: Vec<BattleCommandDto>,
    },
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
    SimultaneousSwitch {
        switches: Vec<SwitchDto>,
    },
    EndOfTurn,
    Faint {
        slot: FieldSlotDto,
    },
    #[serde(rename_all = "camelCase")]
    MegaEvolution {
        slot: FieldSlotDto,
        into: String,
    },
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
    ChargingMove {
        user: FieldSlotDto,
        r#move: String,
    },
    #[serde(rename_all = "camelCase")]
    MustRecharge {
        slot: FieldSlotDto,
    },
    #[serde(rename_all = "camelCase")]
    SingleMoveOrTurn {
        slot: FieldSlotDto,
        r#move: String,
    },
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
    MoveFailed {
        slot: FieldSlotDto,
    },
    Blocked {
        target: FieldSlotDto,
    },
    #[serde(rename_all = "camelCase")]
    HitCount {
        target: FieldSlotDto,
        hits: u8,
    },
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
    TeamStatusCured {
        side: PlayerDto,
    },
    #[serde(rename_all = "camelCase")]
    BoostChanged {
        target: FieldSlotDto,
        stat: String,
        stages: i8,
    },
    #[serde(rename_all = "camelCase")]
    BoostsCleared {
        target: FieldSlotDto,
    },
    #[serde(rename_all = "camelCase")]
    BoostsInverted {
        target: FieldSlotDto,
    },
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
    WeatherChanged {
        weather: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    TerrainChanged {
        terrain: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    PseudoWeatherStart {
        effect: String,
    },
    #[serde(rename_all = "camelCase")]
    PseudoWeatherEnd {
        effect: String,
    },
    #[serde(rename_all = "camelCase")]
    SideConditionStart {
        side: PlayerDto,
        condition: String,
    },
    #[serde(rename_all = "camelCase")]
    SideConditionEnd {
        side: PlayerDto,
        condition: String,
    },
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
    ItemRevealed {
        slot: FieldSlotDto,
        item: String,
    },
    #[serde(rename_all = "camelCase")]
    ItemGained {
        slot: FieldSlotDto,
        item: String,
    },
    #[serde(rename_all = "camelCase")]
    ItemLost {
        slot: FieldSlotDto,
        item: String,
        consumed: bool,
    },
    #[serde(rename_all = "camelCase")]
    AbilityRevealed {
        slot: FieldSlotDto,
        ability: String,
    },
    #[serde(rename_all = "camelCase")]
    AnticipationShudder {
        slot: FieldSlotDto,
    },
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
    /// Send an empty string when the matching team mode is `meta`.
    pub p1_team: String,
    pub p2_team: String,
    /// Selects a pasted team or a generated team.
    #[serde(default = "default_team_mode")]
    pub p1_team_mode: String,
    #[serde(default = "default_team_mode")]
    pub p2_team_mode: String,
    /// Makes generated teams reproducible.
    /// `None` selects a new random seed.
    pub meta_seed: Option<u64>,
    pub active_per_side: u8,
    pub brought_per_side: u8,
    /// The roster size of the format.
    /// A generated team gets this many Pokemon, and team preview brings
    /// `brought_per_side` of them.
    /// The default of 6 keeps an older client working.
    #[serde(default = "default_roster_size")]
    pub total_per_side: u8,
    #[serde(default = "default_true")]
    pub stat_points: bool,
    #[serde(default = "default_true")]
    pub consider_crit: bool,
    /// Sets all inferred opponent IVs to the Champions default of 31.
    #[serde(default = "default_true")]
    pub force_max_ivs: bool,
    /// Whether the selected regulation permits each once-per-battle mechanic.
    /// Both default to true for older clients and saved formats.
    #[serde(default = "default_true")]
    pub tera_enabled: bool,
    #[serde(default = "default_true")]
    pub mega_enabled: bool,
    #[serde(default = "default_damage_rolls")]
    pub damage_rolls: u8,
    /// Selects the initial opponent information for P1.
    /// A closed sheet shows only opponent species at preview.
    #[serde(default = "default_info_mode")]
    pub information_mode: String,
    /// Permitted item slugs after format bans.
    /// An empty list permits all items.
    /// The server rejects an unknown slug with HTTP 422.
    #[serde(default)]
    pub legal_items: Vec<String>,
}

fn default_true() -> bool {
    true
}

fn default_damage_rolls() -> u8 {
    16
}

/// The Champions roster size.
fn default_roster_size() -> u8 {
    6
}

fn default_team_mode() -> String {
    "sheet".to_string()
}

fn default_info_mode() -> String {
    "closedSheet".to_string()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBattleResponse {
    pub battle_id: String,
    /// P1's fog-of-war view of the battle.
    pub state: BattleView,
    /// P2's masked view of the same battle.
    pub state_p2: BattleView,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetBattleResponse {
    pub state: BattleView,
    pub state_p2: BattleView,
    /// Turn events masked for P1.
    pub log: Vec<TurnLogEntry>,
    /// Turn events masked for P2.
    pub log_p2: Vec<TurnLogEntry>,
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
    pub state_p2: BattleView,
    /// This turn's events masked for P1.
    pub events: Vec<EventNode>,
    /// This turn's events masked for P2.
    pub events_p2: Vec<EventNode>,
    pub probability: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub message: String,
}

// ── Tracker mode ─────────────────────────────────────────────────────────────
// Tracker sessions record typed events from a real battle.
// They do not simulate an opponent or resolve commands.
// The server converts text to inference events.

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTrackerRequest {
    /// The tracker viewer's complete Showdown teamsheet.
    pub my_team: String,
    /// Opponent teamsheet or comma-delimited species names.
    /// The server keeps only species from this value.
    pub opponent: String,
    pub active_per_side: u8,
    pub brought_per_side: u8,
    #[serde(default = "default_true")]
    pub stat_points: bool,
    #[serde(default = "default_true")]
    pub force_max_ivs: bool,
    #[serde(default = "default_true")]
    pub tera_enabled: bool,
    #[serde(default = "default_true")]
    pub mega_enabled: bool,
    /// Tracker information mode.
    /// Tracker mode does not permit perfect information.
    #[serde(default = "default_info_mode")]
    pub information_mode: String,
    #[serde(default)]
    pub legal_items: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTrackerResponse {
    pub tracker_id: String,
    pub state: BattleView,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTrackerResponse {
    pub state: BattleView,
    pub log: Vec<TurnLogEntry>,
    /// Complete committed tracker text.
    /// An empty string means that no turn is committed.
    pub script: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackerEventsRequest {
    /// One or more complete turns.
    /// Each turn ends with `endofturn`.
    pub text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackerParseErrorDto {
    pub line: usize,
    pub message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackerEventsResponse {
    pub state: BattleView,
    /// New committed turns.
    /// Append them to the current log.
    pub log_delta: Vec<TurnLogEntry>,
}

/// Contains tracker text for an incomplete turn preview.
/// This text does not require `endofturn`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackerPreviewRequest {
    pub text: String,
}

/// Contains a temporary structural view for an incomplete turn.
/// The server does not store this view.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackerPreviewResponse {
    pub state: BattleView,
    /// Parsed preview events with generated guaranteed effects.
    pub events: Vec<EventNode>,
}

/// Contains autocomplete names for both match rosters.
/// Moves and abilities come from the roster species.
/// The frontend supplies items from its catalog.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackerCompletionsDto {
    pub species: Vec<String>,
    pub moves: Vec<String>,
    pub abilities: Vec<String>,
}

/// Contains alphabetical teamsheet species names.
/// The setup page uses this list before it creates a session.
/// The server removes battle-only forms.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeciesListDto {
    pub species: Vec<String>,
}

// ── Benchmarking ─────────────────────────────────────────────────────────────
// The benchmark endpoint runs the same grids as the offline benchmarks.
// It runs the three sweeps in sequence.
// Each sweep sends progress and then a result or failure.
// A final `done` event closes the stream.
// Sequential runs prevent contended measurements.

/// Identifies the sweep for one event.
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub enum BenchmarkSweepDto {
    TurnSpeed,
    Inference,
    Solver,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkProgressDto {
    pub stage: BenchmarkSweepDto,
    pub completed: usize,
    pub total: usize,
}

/// Contains the rows from one completed sweep.
/// The tag separates an empty result from an incomplete sweep.
#[derive(Serialize)]
#[serde(tag = "sweep", rename_all = "camelCase")]
pub enum BenchmarkResultDto {
    #[serde(rename_all = "camelCase")]
    TurnSpeed { rows: Vec<TurnSpeedRowDto> },
    #[serde(rename_all = "camelCase")]
    Inference { rows: Vec<InferenceRowDto> },
    #[serde(rename_all = "camelCase")]
    Solver { rows: Vec<SolverRowDto> },
}

/// Reports one failed sweep.
/// Later sweeps continue.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkSweepErrorDto {
    pub sweep: BenchmarkSweepDto,
    pub message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSpeedRowDto {
    pub scenario: String,
    pub mode: String,
    pub rolls: u8,
    pub crit: bool,
    pub avg_time_secs: f64,
    pub avg_branches: usize,
    pub pairings: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InferenceRowDto {
    pub scenario: String,
    pub information_mode: String,
    pub calls: u64,
    pub avg_time_secs: f64,
    pub contradictions: u64,
    /// First caught contradiction message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contradiction_sample: Option<String>,
}

/// Contains one solver benchmark configuration.
/// `avg_turns_simulated` measures the main solver cost.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SolverRowDto {
    pub scenario: String,
    pub algorithm: String,
    pub depth: u8,
    pub rolls: u8,
    pub chance: String,
    /// Joint-action cap in force, if any. Absent means the full action set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_cap: Option<usize>,
    pub avg_time_secs: f64,
    pub avg_nodes: f64,
    pub avg_turns_simulated: f64,
    pub avg_cells_evaluated: f64,
    pub avg_cells_total: f64,
    pub avg_lps: f64,
    pub pairings: usize,
    /// Why the cell was not attempted. Absent when it ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
}
