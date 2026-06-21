use crate::data::ability::Ability;
use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use std::collections::HashMap;
use std::fs;

pub type PokemonBoostTable = [i8; 7]; // atk, def, spa, spd, spe, accuracy, evasion

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PokemonType {
    Normal,
    Fire,
    Water,
    Electric,
    Grass,
    Ice,
    Fighting,
    Poison,
    Ground,
    Flying,
    Psychic,
    Bug,
    Rock,
    Ghost,
    Dragon,
    Dark,
    Steel,
    Fairy,
}

#[derive(Debug)]
pub enum AccuracyType {
    True,
    Percent(u8),
}

#[derive(Debug)]
pub enum MoveCategory {
    Physical,
    Special,
    Status,
}

#[derive(Debug, PartialEq)]
pub enum MoveTarget {
    AdjacentAlly,       //Targets Teammates
    AdjacentAllyOrSelf, //Targets teammates or self
    AdjacentFoe,        //Targets enemies
    All,                //Targets the whole field at once
    AllAdjacent,        //Targets all pokemon except self (teammates and foes)
    AllAdjacentFoes,    //Targets all pokemon on enemy side
    Allies,             //Targets all pokemon on your side
    AllySide,           //Targets all pokemon on your side
    AllyTeam,           //Targets all pokemon on your side (same as above)
    Any,                //Can target any individual mon on the field
    FoeSide,            //Targets all opposing pokemon at once
    Normal,             // Can target any individual mon, excluding self
    RandomNormal,       //Chooses a target at random
    Scripted, //Ignore this for now, moves that reflect damage that the user takes (mirror armor etc.)
    SelfTarget, //Must target itself
}

#[derive(Debug, Clone)]
pub enum MoveFlag {
    BypassSub,
    Bite,
    Bullet,
    CantUseTwice,
    Charge,
    Contact,
    Dance,
    Defrost,
    Distance,
    FailCopyCat,
    FailEncore,
    FailInstruct,
    FailMeFirst,
    FailMimic,
    FutureMove,
    Gravity,
    Heal,
    Metronome,
    Mirror,
    MustPressure,
    NoAssist,
    NoParentalBond,
    NoSketch,
    NoSleepTalk,
    PledgeCombo,
    Powder,
    Protect,
    Pulse,
    Punch,
    Recharge,
    Reflectable,
    Slicing,
    Snatch,
    Sound,
    Wind,
}

#[derive(Debug)]
pub enum DamageOverride {
    Number(u16),
    Level,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PokemonStat {
    Atk,
    Def,
    SpA,
    SpD,
    Spe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelfSwitchType {
    ShedTail,
    BatonPass,
    Normal,
    None,
}

#[derive(Debug)]
pub enum SelfDestructType {
    Always,
    IfHit,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Status {
    Burn,
    Poison,
    // ToxicPoison stores the number of turns it has been active (starts at 0)
    ToxicPoison(u8),
    Paralysis,
    // Sleep stores number of turns asleep (starts at 0)
    Sleep(u8),
    // Frozen stores number of turns frozen (starts at 0)
    Frozen(u8),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VolatileStatus {
    Flinch,
    AquaRing,
    Attract,
    Confusion,
    BanefulBunker,
    Bide,
    PartiallyTrapped(u8),
    Trapped(u8),
    MustRecharge,
    BurningBulwark,
    Charge,
    Curse,
    DefenseCurl,
    DestinyBond,
    Protect,
    Disable(PokemonMove),
    /// The holder cannot select the named move on consecutive turns (e.g. Gigaton Hammer, Blood Moon).
    /// Set as MoveStatus with duration 2 so `decrement_move_statuses` clears it after one turn.
    CantUseRepeatedly(PokemonMove),
    /// Critical-hit boost from Dragon Cheer. The payload stores the crit-stage bonus
    /// (1, or 2 when the boosted ally was Dragon-type at the time the move was used);
    /// it is locked in at application and does not change if the ally's type changes.
    DragonCheer(u8),
    Electrify,
    Embargo,
    Encore(PokemonMove),
    Endure,
    FlashFire,
    FocusEnergy,
    FollowMe,
    Foresight,
    GastroAcid,
    GlaiveRush,
    Grudge,
    HealBlock,
    HelpingHand,
    Imprison,
    Ingrain,
    KingsShield,
    LaserFocus,
    /// User has locked onto a specific target (from Lock-On / Mind Reader). Payload is the
    /// target's `mon_id`. Stored as MoveStatus(_, 2) so it persists for exactly one active
    /// turn after application (decremented to 1 at turn-start, then removed next decrement).
    LockedOn(u8),
    LeechSeed,
    MagicCoat,
    MagnetRise,
    MaxGuard,
    Minimize,
    MiracleEye,
    NightMare,
    NoRetreat,
    Obstruct,
    OctoLock,
    ChoiceLock(PokemonMove),
    /// User is locked into a rampaging move (Thrash/Outrage/Petal Dance/Raging Fury).
    /// Payload: the move being rampaged. Stored as MoveStatus(_, turns_remaining) where
    /// turns_remaining counts *additional* turns still to fire (0 = this was the last one,
    /// confusion fires at end of it).
    LockedMove(crate::data::pokemon_move::PokemonMove),
    SemiInvulnerable(PokemonMove),
    /// Set at turn-start before any action resolves; any contact move hitting the holder burns
    /// the attacker. Cleared at end-of-turn or immediately after Beak Blast fires.
    BeakBlastCharging,
    /// Set at turn-start for a Focus Punch user (priority −3). If the holder is hit by a
    /// damaging move before their action resolves, Focus Punch fails (without consuming PP).
    /// Cleared at end-of-turn (TurnStatus, duration 1).
    FocusPunchCharging,
    Powder,
    PowerShift,
    PowerTrick,
    SpeedSwap(u16),
    PerishSong,
    Rage,
    RagePowder,
    Roost,
    SaltCure,
    /// Stores the substitute's current HP. Zero means absent/broken (should be removed).
    Substitute(u16),
    SilkTrap,
    SmackDown,
    Snatch,
    SparklingAria,
    SpikyShield,
    SkyDrop,
    Spotlight,
    /// Stockpile charge level (1–3). Stored as TurnStatus(_, 0) so the payload — not a turn
    /// counter — carries the level; it persists until Spit Up / Swallow consume it or the
    /// user switches out.
    Stockpile(u8),
    /// Carries the fainted-ally count (1–5) snapshotted at switch-in. Stored as
    /// TurnStatus(_, 0) so it lasts indefinitely on the field and is wiped on switch-out.
    SupremeOverlord(u8),
    SyrupBomb,
    TarShot,
    Taunt,
    Telekinesis,
    ThroatChop,
    Torment,
    Uproar,
    Yawn,
    /// Marks that the Grass type currently on this Pokémon was *added* by Forest's Curse
    /// (as opposed to being one of its natural types). Used so Trick-or-Treat knows to
    /// replace the added type rather than appending a fourth type. Cleared on switch-out.
    ForestsCurse,
    /// Marks that the Ghost type currently on this Pokémon was *added* by Trick-or-Treat.
    /// Mirror of `ForestsCurse`. Cleared on switch-out.
    TrickorTreat,
    /// Set after Protean / Libero fires once per switch-in. Stored as TurnStatus(_, 0) so it
    /// persists indefinitely on the field and is wiped automatically on switch-out.
    ProteanActivated,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SideCondition {
    AuroraVeil,
    Reflect,
    CraftyShield,
    LightScreen,
    LuckyChant,
    MatBlock,
    Mist,
    QuickGuard,
    SafeGuard,
    /// Entry hazard. Carries the current layer count (1..=3).
    Spikes(u8),
    StealthRock,
    /// Entry hazard. Carries the `mon_id` of the Pokémon that set it (for Mirror Armor
    /// reflection), or `None` when the source is untracked.
    StickyWeb(Option<u8>),
    TailWind,
    /// Entry hazard. Carries the current layer count (1..=2).
    ToxicSpikes(u8),
    WideGuard,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SlotCondition {
    /// Pending Future Sight or Doom Desire. Fires at end of turn when `turns_remaining`
    /// hits 0. All attacker-side values are snapshotted at queue time so damage is stable
    /// even if the original user switches out before impact. The live target's stats/types/
    /// items are used on the defensive side.
    FutureMove {
        move_name: PokemonMove,
        /// True when the attacker is Player 1.
        attacker_is_p1: bool,
        attacker_slot_index: u8,
        /// mon_id of the attacker; lets the resolver verify if the same mon is still in slot.
        attacker_mon_id: u8,
        /// Raw Sp.Atk stat value (mon.stats[3]) at queue time, before boosts/ability/item.
        snapshot_raw_spa: u16,
        /// Sp.Atk boost stage at queue time (mon.boosts[2]). Combined with snapshot_raw_spa
        /// in effective_stat to reproduce the correct offensive value.
        snapshot_spa_boost: i8,
        snapshot_level: u8,
        snapshot_type1: Option<PokemonType>,
        snapshot_type2: Option<PokemonType>,
        snapshot_ability: Ability,
        snapshot_item: Item,
        turns_remaining: u8,
    },
    HealingWish,
    LunarDance,
    RevivalBlessing,
    /// Pending Wish: `heal` HP (½ the wisher's max HP) is restored to whatever Pokémon
    /// occupies this slot when `turns_remaining` reaches 0 at end of turn.
    Wish {
        heal: u16,
        turns_remaining: u8,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PseudoWeather {
    FairyLock,
    Gravity,
    IonDeluge,
    MagicDeluge,
    MudSport,
    TrickRoom,
    WaterSport,
    WonderRoom,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Terrain {
    ElectricTerrain,
    GrassyTerrain,
    MistyTerrain,
    PsychicTerrain,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Weather {
    Rain,
    HeavyRain,
    Sandstorm,
    Snow,
    Sun,
    ExtremeSunlight,
    StrongWinds,
}

#[derive(Debug, Default)]
pub struct HitEffect {
    pub boosts: PokemonBoostTable,
    pub status: Option<Status>,
    pub volatile_status: Option<VolatileStatus>,
    pub slot_condition: Option<SlotCondition>,
    pub side_condition: Option<SideCondition>,
    pub pseudo_weather: Option<PseudoWeather>,
    pub terrain: Option<Terrain>,
    pub weather: Option<Weather>,
}

#[derive(Debug)]
pub struct PokemonSecondaryEffect {
    pub chance: u8,
    pub effect: HitEffect,
    /// Mutually-exclusive effects chosen uniformly at random when the secondary
    /// fires (e.g. Tri Attack's burn/freeze/paralyze, Dire Claw's
    /// poison/paralyze/sleep). Each entry may carry a status or a volatile.
    /// When empty, `effect` is applied as usual.
    pub random_choices: Vec<HitEffect>,
}

impl PokemonSecondaryEffect {
    /// A secondary with a single deterministic effect (no random choices).
    fn simple(chance: u8, effect: HitEffect) -> Self {
        PokemonSecondaryEffect {
            chance,
            effect,
            random_choices: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct MoveData {
    pub name: PokemonMove,
    pub base_power: u16,
    pub accuracy: AccuracyType,
    pub target: MoveTarget,
    pub secondaries: Vec<PokemonSecondaryEffect>,
    pub self_secondaries: Vec<PokemonSecondaryEffect>,
    pub pp: u8,

    pub category: MoveCategory,
    pub pokemon_type: PokemonType,
    pub priority: i8,
    pub flags: Vec<MoveFlag>,

    // Hit Effects
    pub ohko: bool,
    pub thaws_target: bool,
    pub heal_fraction: [u8; 2],
    pub force_switch: bool,
    pub self_switch: SelfSwitchType,
    pub self_boost: PokemonBoostTable,
    pub self_destruct: SelfDestructType,
    pub breaks_protect: bool,
    pub recoil_fraction: [u8; 2],
    pub drain_fraction: [u8; 2],
    pub mind_blown_recoil: bool,
    pub struggle_recoil: bool,

    // Hit Modifiers
    pub crit_ratio: u8,
    pub foul_play: bool,

    // Other Mods
    pub ignore_ability: bool,
    pub ignore_defense_boosts: bool,
    pub ignore_evasion: bool,
    pub ignore_immunity: Vec<PokemonType>,

    pub multihit_range: [u8; 2],
    pub multihit_accuracy: bool,

    pub sleep_usable: bool,
    pub has_crash_damage: bool,
    pub damage_override: DamageOverride,

    pub stalling_move: bool,
    pub override_offensive_stat: Option<PokemonStat>,
    pub override_defensive_stat: Option<PokemonStat>,
}

#[derive(Debug)]
pub struct PokemonData {
    pub species: Species,
    pub types: Vec<PokemonType>,
    pub base_stats: [u16; 6],
    pub weight: u16,
    pub primary_ability: Option<Ability>,
    pub base_species: Option<Species>,
    pub forme: Option<Species>,
    pub required_item: Option<String>,
    pub battle_only: Option<Species>,
    pub default_gender: crate::state::pokemon::PokemonGender,
}

// --- Helpers ---

pub fn parse_type(s: &str) -> Option<PokemonType> {
    match s {
        "Normal" => Some(PokemonType::Normal),
        "Fire" => Some(PokemonType::Fire),
        "Water" => Some(PokemonType::Water),
        "Electric" => Some(PokemonType::Electric),
        "Grass" => Some(PokemonType::Grass),
        "Ice" => Some(PokemonType::Ice),
        "Fighting" => Some(PokemonType::Fighting),
        "Poison" => Some(PokemonType::Poison),
        "Ground" => Some(PokemonType::Ground),
        "Flying" => Some(PokemonType::Flying),
        "Psychic" => Some(PokemonType::Psychic),
        "Bug" => Some(PokemonType::Bug),
        "Rock" => Some(PokemonType::Rock),
        "Ghost" => Some(PokemonType::Ghost),
        "Dragon" => Some(PokemonType::Dragon),
        "Dark" => Some(PokemonType::Dark),
        "Steel" => Some(PokemonType::Steel),
        "Fairy" => Some(PokemonType::Fairy),
        _ => None,
    }
}

fn parse_target(s: &str) -> MoveTarget {
    match s {
        "adjacentAlly" => MoveTarget::AdjacentAlly,
        "adjacentAllyOrSelf" => MoveTarget::AdjacentAllyOrSelf,
        "adjacentFoe" => MoveTarget::AdjacentFoe,
        "all" => MoveTarget::All,
        "allAdjacent" => MoveTarget::AllAdjacent,
        "allAdjacentFoes" => MoveTarget::AllAdjacentFoes,
        "allies" => MoveTarget::Allies,
        "allySide" => MoveTarget::AllySide,
        "allyTeam" => MoveTarget::AllyTeam,
        "any" => MoveTarget::Any,
        "foeSide" => MoveTarget::FoeSide,
        "normal" => MoveTarget::Normal,
        "randomNormal" => MoveTarget::RandomNormal,
        "scripted" => MoveTarget::Scripted,
        "self" => MoveTarget::SelfTarget,
        _ => MoveTarget::Normal,
    }
}

fn parse_category(s: &str) -> MoveCategory {
    match s {
        "Physical" => MoveCategory::Physical,
        "Special" => MoveCategory::Special,
        _ => MoveCategory::Status,
    }
}

fn parse_flag(s: &str) -> Option<MoveFlag> {
    match s {
        "bypasssub" => Some(MoveFlag::BypassSub),
        "bite" => Some(MoveFlag::Bite),
        "bullet" => Some(MoveFlag::Bullet),
        "cantusetwice" => Some(MoveFlag::CantUseTwice),
        "charge" => Some(MoveFlag::Charge),
        "contact" => Some(MoveFlag::Contact),
        "dance" => Some(MoveFlag::Dance),
        "defrost" => Some(MoveFlag::Defrost),
        "distance" => Some(MoveFlag::Distance),
        "failcopycat" => Some(MoveFlag::FailCopyCat),
        "failencore" => Some(MoveFlag::FailEncore),
        "failinstruct" => Some(MoveFlag::FailInstruct),
        "failmefirst" => Some(MoveFlag::FailMeFirst),
        "failmimic" => Some(MoveFlag::FailMimic),
        "futuremove" => Some(MoveFlag::FutureMove),
        "gravity" => Some(MoveFlag::Gravity),
        "heal" => Some(MoveFlag::Heal),
        "metronome" => Some(MoveFlag::Metronome),
        "mirror" => Some(MoveFlag::Mirror),
        "mustpressure" => Some(MoveFlag::MustPressure),
        "noassist" => Some(MoveFlag::NoAssist),
        "noparentalbond" => Some(MoveFlag::NoParentalBond),
        "nosketch" => Some(MoveFlag::NoSketch),
        "nosleeptalk" => Some(MoveFlag::NoSleepTalk),
        "pledgecombo" => Some(MoveFlag::PledgeCombo),
        "powder" => Some(MoveFlag::Powder),
        "protect" => Some(MoveFlag::Protect),
        "pulse" => Some(MoveFlag::Pulse),
        "punch" => Some(MoveFlag::Punch),
        "recharge" => Some(MoveFlag::Recharge),
        "reflectable" => Some(MoveFlag::Reflectable),
        "slicing" => Some(MoveFlag::Slicing),
        "snatch" => Some(MoveFlag::Snatch),
        "sound" => Some(MoveFlag::Sound),
        "wind" => Some(MoveFlag::Wind),
        _ => None,
    }
}

fn parse_nvstatus(s: &str) -> Option<Status> {
    match s {
        "brn" => Some(Status::Burn),
        "psn" => Some(Status::Poison),
        "tox" => Some(Status::ToxicPoison(0)),
        "par" => Some(Status::Paralysis),
        "slp" => Some(Status::Sleep(0)),
        "frz" => Some(Status::Frozen(0)),
        _ => None,
    }
}

fn parse_volatile(s: &str) -> Option<VolatileStatus> {
    match s {
        "flinch" => Some(VolatileStatus::Flinch),
        "aquaring" => Some(VolatileStatus::AquaRing),
        "attract" => Some(VolatileStatus::Attract),
        "confusion" => Some(VolatileStatus::Confusion),
        "banefulbunker" => Some(VolatileStatus::BanefulBunker),
        "bide" => Some(VolatileStatus::Bide),
        "partiallytrapped" => Some(VolatileStatus::PartiallyTrapped(u8::MAX)),
        "mustrecharge" => Some(VolatileStatus::MustRecharge),
        "burningbulwark" => Some(VolatileStatus::BurningBulwark),
        "charge" => Some(VolatileStatus::Charge),
        "curse" => Some(VolatileStatus::Curse),
        "defensecurl" => Some(VolatileStatus::DefenseCurl),
        "destinybond" => Some(VolatileStatus::DestinyBond),
        "protect" => Some(VolatileStatus::Protect),
        "disable" => Some(VolatileStatus::Disable(PokemonMove::Struggle)),
        "dragoncheer" => Some(VolatileStatus::DragonCheer(1)),
        "electrify" => Some(VolatileStatus::Electrify),
        "embargo" => Some(VolatileStatus::Embargo),
        // Encore is applied via a dedicated handler that captures the target's last move; this
        // placeholder only keeps `status_move_changed_state` detection working during parsing.
        "encore" => Some(VolatileStatus::Encore(PokemonMove::Struggle)),
        "endure" => Some(VolatileStatus::Endure),
        "focusenergy" => Some(VolatileStatus::FocusEnergy),
        "followme" => Some(VolatileStatus::FollowMe),
        "foresight" => Some(VolatileStatus::Foresight),
        "gastroacid" => Some(VolatileStatus::GastroAcid),
        "glaiverush" => Some(VolatileStatus::GlaiveRush),
        "grudge" => Some(VolatileStatus::Grudge),
        "healblock" => Some(VolatileStatus::HealBlock),
        "helpinghand" => Some(VolatileStatus::HelpingHand),
        "imprison" => Some(VolatileStatus::Imprison),
        "ingrain" => Some(VolatileStatus::Ingrain),
        "kingsshield" => Some(VolatileStatus::KingsShield),
        "laserfocus" => Some(VolatileStatus::LaserFocus),
        "leechseed" => Some(VolatileStatus::LeechSeed),
        "magiccoat" => Some(VolatileStatus::MagicCoat),
        "magnetrise" => Some(VolatileStatus::MagnetRise),
        "maxguard" => Some(VolatileStatus::MaxGuard),
        "minimize" => Some(VolatileStatus::Minimize),
        "miracleeye" => Some(VolatileStatus::MiracleEye),
        "nightmare" => Some(VolatileStatus::NightMare),
        "noretreat" => Some(VolatileStatus::NoRetreat),
        "perishsong" => Some(VolatileStatus::PerishSong),
        "obstruct" => Some(VolatileStatus::Obstruct),
        "octolock" => Some(VolatileStatus::OctoLock),

        "powder" => Some(VolatileStatus::Powder),
        "powershift" => Some(VolatileStatus::PowerShift),
        "powertrick" => Some(VolatileStatus::PowerTrick),
        "rage" => Some(VolatileStatus::Rage),
        "ragepowder" => Some(VolatileStatus::RagePowder),
        "roost" => Some(VolatileStatus::Roost),
        "saltcure" => Some(VolatileStatus::SaltCure),
        "substitute" => Some(VolatileStatus::Substitute(0)), // HP set by code, not parser
        "silktrap" => Some(VolatileStatus::SilkTrap),
        "smackdown" => Some(VolatileStatus::SmackDown),
        "snatch" => Some(VolatileStatus::Snatch),
        "sparklingaria" => Some(VolatileStatus::SparklingAria),
        "spikyshield" => Some(VolatileStatus::SpikyShield),
        "skydrop" => Some(VolatileStatus::SkyDrop),
        "spotlight" => Some(VolatileStatus::Spotlight),
        "stockpile" => Some(VolatileStatus::Stockpile(0)),
        "syrupbomb" => Some(VolatileStatus::SyrupBomb),
        "tarshot" => Some(VolatileStatus::TarShot),
        "taunt" => Some(VolatileStatus::Taunt),
        "telekinesis" => Some(VolatileStatus::Telekinesis),
        "throatchop" => Some(VolatileStatus::ThroatChop),
        "torment" => Some(VolatileStatus::Torment),
        "uproar" => Some(VolatileStatus::Uproar),
        "yawn" => Some(VolatileStatus::Yawn),
        _ => None,
    }
}

fn parse_side_condition(s: &str) -> Option<SideCondition> {
    match s {
        "auroraveil" => Some(SideCondition::AuroraVeil),
        "reflect" => Some(SideCondition::Reflect),
        "craftyshield" => Some(SideCondition::CraftyShield),
        "lightscreen" => Some(SideCondition::LightScreen),
        "luckychant" => Some(SideCondition::LuckyChant),
        "matblock" => Some(SideCondition::MatBlock),
        "mist" => Some(SideCondition::Mist),
        "quickguard" => Some(SideCondition::QuickGuard),
        "safeguard" => Some(SideCondition::SafeGuard),
        "spikes" => Some(SideCondition::Spikes(1)),
        "stealthrock" => Some(SideCondition::StealthRock),
        "stickyweb" => Some(SideCondition::StickyWeb(None)),
        "tailwind" => Some(SideCondition::TailWind),
        "toxicspikes" => Some(SideCondition::ToxicSpikes(1)),
        "wideguard" => Some(SideCondition::WideGuard),
        _ => None,
    }
}

fn parse_terrain(s: &str) -> Option<Terrain> {
    match normalize_dex_id(s).as_str() {
        "electricterrain" => Some(Terrain::ElectricTerrain),
        "grassyterrain" => Some(Terrain::GrassyTerrain),
        "mistyterrain" => Some(Terrain::MistyTerrain),
        "psychicterrain" => Some(Terrain::PsychicTerrain),
        _ => None,
    }
}

fn parse_weather_val(s: &str) -> Option<Weather> {
    match normalize_dex_id(s).as_str() {
        "raindance" => Some(Weather::Rain),
        "primordialsea" => Some(Weather::HeavyRain),
        "sunnyday" => Some(Weather::Sun),
        "desolateland" => Some(Weather::ExtremeSunlight),
        "sandstorm" | "sandsear" => Some(Weather::Sandstorm),
        "hail" | "snowscape" | "snow" => Some(Weather::Snow),
        "deltastream" => Some(Weather::StrongWinds),
        _ => None,
    }
}

fn parse_pseudo_weather(s: &str) -> Option<PseudoWeather> {
    match normalize_dex_id(s).as_str() {
        "fairylock" => Some(PseudoWeather::FairyLock),
        "gravity" => Some(PseudoWeather::Gravity),
        "iondeluge" => Some(PseudoWeather::IonDeluge),
        "magicroom" => Some(PseudoWeather::MagicDeluge),
        "mudsport" => Some(PseudoWeather::MudSport),
        "trickroom" => Some(PseudoWeather::TrickRoom),
        "watersport" => Some(PseudoWeather::WaterSport),
        "wonderroom" => Some(PseudoWeather::WonderRoom),
        _ => None,
    }
}

fn parse_slot_condition(s: &str) -> Option<SlotCondition> {
    match s {
        // Future Sight and Doom Desire are set via hand-coded effect blocks with full
        // snapshot data; this parser stub is intentionally omitted so no field-less
        // FutureMove variant is ever constructed from the dex.
        "healingwish" => Some(SlotCondition::HealingWish),
        "lunardance" => Some(SlotCondition::LunarDance),
        "revivalblessing" => Some(SlotCondition::RevivalBlessing),
        // Wish is applied via a dedicated move handler that computes the heal amount from the
        // user's max HP; this placeholder only marks the move as state-changing during parsing.
        "wish" => Some(SlotCondition::Wish {
            heal: 0,
            turns_remaining: 0,
        }),
        _ => None,
    }
}

fn empty_hit_effect() -> HitEffect {
    HitEffect {
        boosts: [0; 7],
        status: None,
        volatile_status: None,
        slot_condition: None,
        side_condition: None,
        pseudo_weather: None,
        terrain: None,
        weather: None,
    }
}

/// Parse a Showdown `this.sample([...])` call (used in `onHit` bodies for moves
/// that randomly apply one of several effects, e.g. Tri Attack / Dire Claw) into a
/// list of single-effect `HitEffect`s, one per choice. Each quoted token is mapped
/// to a non-volatile status first, falling back to a volatile status. Tokens that
/// match neither are skipped. Returns an empty Vec if there is no `sample([...])`.
fn parse_sample_effects(text: &str) -> Vec<HitEffect> {
    let mut choices = Vec::new();
    let Some(rel) = text.find("sample(") else {
        return choices;
    };
    let after = &text[rel + "sample(".len()..];
    let Some(open) = after.find('[') else {
        return choices;
    };
    let Some(close) = after[open..].find(']') else {
        return choices;
    };
    let inner = &after[open + 1..open + close];
    for token in inner.split(',') {
        let token = token
            .trim()
            .trim_matches(|c: char| c == '\'' || c == '"')
            .trim();
        if token.is_empty() {
            continue;
        }
        let mut effect = empty_hit_effect();
        if let Some(status) = parse_nvstatus(token) {
            effect.status = Some(status);
        } else if let Some(volatile) = parse_volatile(token) {
            effect.volatile_status = Some(volatile);
        } else {
            continue;
        }
        choices.push(effect);
    }
    choices
}

/// Extracts the `self: { ... }` sub-block from a block of text.
/// Returns (text_without_self_block, Some(self_block_inner_text)).
fn extract_self_subblock(text: &str) -> (String, Option<String>) {
    let mut search_start = 0;
    while let Some(rel_pos) = text[search_start..].find("self:") {
        let abs_pos = search_start + rel_pos;
        // Ensure it's a word boundary (not part of "selfSwitch:", "selfdestruct:", etc.)
        let prev_char = if abs_pos == 0 {
            ' '
        } else {
            text[..abs_pos].chars().last().unwrap_or(' ')
        };
        let after = text[abs_pos + 5..].trim_start();
        if (!prev_char.is_alphanumeric() && prev_char != '_') && after.starts_with('{') {
            // Find the matching closing brace
            let brace_start = abs_pos + 5 + text[abs_pos + 5..].find('{').unwrap();
            let mut depth = 0i32;
            let mut end_pos = brace_start;
            for (j, c) in text[brace_start..].char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end_pos = brace_start + j;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let self_inner = text[brace_start + 1..end_pos].to_string();
            let rest = format!("{} {}", &text[..abs_pos], &text[end_pos + 1..]);
            return (rest, Some(self_inner));
        }
        search_start = abs_pos + 5;
    }
    (text.to_string(), None)
}

/// Parse boosts/status/volatileStatus from a text fragment into a HitEffect.
fn parse_effect_from_text(text: &str) -> HitEffect {
    let mut effect = empty_hit_effect();
    if let Some(s) = extract_quoted(text, "status") {
        effect.status = parse_nvstatus(&s);
    }
    if let Some(s) = extract_quoted(text, "volatileStatus") {
        effect.volatile_status = parse_volatile(&s);
    }
    if let Some(s) = extract_quoted(text, "sideCondition") {
        effect.side_condition = parse_side_condition(&s);
    }
    if let Some(s) = extract_quoted(text, "terrain") {
        effect.terrain = parse_terrain(&s);
    }
    if let Some(s) = extract_quoted(text, "weather") {
        effect.weather = parse_weather_val(&s);
    }
    if let Some(s) = extract_quoted(text, "pseudoWeather") {
        effect.pseudo_weather = parse_pseudo_weather(&s);
    }
    if let Some(s) = extract_quoted(text, "slotCondition") {
        effect.slot_condition = parse_slot_condition(&s);
    }
    if let Some(bp) = text.find("boosts:") {
        let rest = &text[bp..];
        if let Some(ob) = rest.find('{') {
            let inner = &rest[ob..];
            if let Some(cb) = inner.find('}') {
                effect.boosts = parse_boosts_from_text(&inner[..=cb]);
            }
        }
    }
    effect
}

/// Extract a quoted string value from a line like `name: "Absorb",`
fn extract_quoted(line: &str, key: &str) -> Option<String> {
    let pat = format!("{}: \"", key);
    if let Some(start) = line.find(&pat) {
        let rest = &line[start + pat.len()..];
        if let Some(end) = rest.find('"') {
            return Some(rest[..end].to_string());
        }
    }
    // Also handle single-quoted values
    let pat2 = format!("{}: '", key);
    if let Some(start) = line.find(&pat2) {
        let rest = &line[start + pat2.len()..];
        if let Some(end) = rest.find('\'') {
            return Some(rest[..end].to_string());
        }
    }
    None
}

/// Extract an integer value from a line like `pp: 25,`
fn extract_int<T: std::str::FromStr>(line: &str, key: &str) -> Option<T> {
    let pat = format!("{}: ", key);
    if let Some(start) = line.find(&pat) {
        let rest = &line[start + pat.len()..];
        let val_str: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '-')
            .collect();
        return val_str.parse().ok();
    }
    None
}

/// Extract a boolean or detect `true` value from a line like `accuracy: true,`
fn extract_bool(line: &str, key: &str) -> Option<bool> {
    let pat = format!("{}: ", key);
    if let Some(start) = line.find(&pat) {
        let rest = &line[start + pat.len()..].trim_end_matches(',').trim();
        return match *rest {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        };
    }
    None
}

/// Extract a two-element array like `[1, 2]`
fn extract_array2(line: &str, key: &str) -> Option<[u8; 2]> {
    let pat = format!("{}: [", key);
    if let Some(start) = line.find(&pat) {
        let rest = &line[start + pat.len()..];
        if let Some(end) = rest.find(']') {
            let inner = &rest[..end];
            let nums: Vec<&str> = inner.split(',').collect();
            if nums.len() == 2 {
                if let (Ok(a), Ok(b)) = (nums[0].trim().parse::<u8>(), nums[1].trim().parse::<u8>())
                {
                    return Some([a, b]);
                }
            }
        }
    }
    None
}

/// Parse boosts from a block of text like `{ atk: -1, def: 2 }`
/// Returns [atk, def, spa, spd, spe, accuracy, evasion, 0]
fn parse_boosts_from_text(text: &str) -> PokemonBoostTable {
    let mut boosts = [0i8; 7];
    for part in text.split(',') {
        let part = part.trim();
        if let Some((key, val)) = part.split_once(':') {
            let key = key.trim().trim_matches(|c: char| !c.is_alphanumeric());
            let val: i8 = val
                .trim()
                .trim_matches(|c: char| !c.is_ascii_digit() && c != '-')
                .parse()
                .unwrap_or(0);
            match key {
                "atk" => boosts[0] = val,
                "def" => boosts[1] = val,
                "spa" => boosts[2] = val,
                "spd" => boosts[3] = val,
                "spe" => boosts[4] = val,
                "accuracy" => boosts[5] = val,
                "evasion" => boosts[6] = val,
                _ => {}
            }
        }
    }
    boosts
}

/// Parse flags from text like `{ contact: 1, protect: 1, mirror: 1 }`
fn parse_flags_from_text(text: &str) -> Vec<MoveFlag> {
    let mut flags = Vec::new();
    let mut inner = text.trim();

    if let Some(rest) = inner.strip_prefix("flags:") {
        inner = rest.trim();
    }

    inner = inner
        .trim_start_matches('{')
        .trim_end_matches('}')
        .trim_end_matches(',')
        .trim();

    for part in inner.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        if let Some((key, _)) = part.split_once(':') {
            if let Some(flag) = parse_flag(
                key.trim()
                    .trim_matches(|c: char| c == '{' || c == '}' || c == '\'' || c == '"'),
            ) {
                flags.push(flag);
            }
        }
    }
    flags
}

fn parse_damage_override(s: &str) -> Option<DamageOverride> {
    match s {
        "level" => Some(DamageOverride::Level),
        _ => Some(DamageOverride::Number(s.parse().ok()?)),
    }
}

fn parse_stat(s: &str) -> Option<PokemonStat> {
    match s {
        "atk" => Some(PokemonStat::Atk),
        "def" => Some(PokemonStat::Def),
        "spa" => Some(PokemonStat::SpA),
        "spd" => Some(PokemonStat::SpD),
        "spe" => Some(PokemonStat::Spe),
        _ => None,
    }
}

fn normalize_dex_id(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn extract_first_quoted_value(text: &str) -> Option<String> {
    for quote in ['"', '\''] {
        if let Some(start) = text.find(quote) {
            let rest = &text[start + 1..];
            if let Some(end) = rest.find(quote) {
                return Some(rest[..end].to_string());
            }
        }
    }
    None
}

fn parse_primary_ability_from_text(text: &str) -> Option<Ability> {
    for key in ["\"0\":", "0:"] {
        if let Some(pos) = text.find(key) {
            let rest = &text[pos + key.len()..];
            if let Some(ability) = extract_first_quoted_value(rest) {
                return Some(Ability::from_str(&ability));
            }
        }
    }

    // Fallback: take the first quoted ability-like value if key `0` wasn't found.
    extract_first_quoted_value(text).map(|s| Ability::from_str(&s))
}

/// Split file content into top-level entry blocks.
/// Returns Vec of (key, block_lines) where block_lines are the lines inside the braces.
fn split_entries(content: &str) -> Vec<(String, Vec<String>)> {
    let mut entries = Vec::new();
    let mut depth: i32 = 0;
    let mut current_key = String::new();
    let mut current_lines: Vec<String> = Vec::new();
    let mut in_entry = false;

    for line in content.lines() {
        let trimmed = line.trim();

        let open = trimmed.chars().filter(|&c| c == '{').count() as i32;
        let close = trimmed.chars().filter(|&c| c == '}').count() as i32;

        if depth == 0 && trimmed.contains(": {") && !trimmed.starts_with("//") {
            // Entry start
            let key = trimmed
                .split(':')
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string();
            current_key = key;
            current_lines.clear();
            in_entry = true;
            depth += open;
            continue;
        }

        depth += open - close;

        if in_entry {
            if depth <= 0 {
                // Entry end
                entries.push((current_key.clone(), current_lines.clone()));
                in_entry = false;
                depth = 0;
            } else {
                current_lines.push(trimmed.to_string());
            }
        }
    }

    entries
}

/// Skip lines that are JavaScript function bodies by detecting function patterns.
fn is_function_line(line: &str) -> bool {
    let line = line.trim();
    // Detect function definitions like `onHit(target) {`, `basePowerCallback(pokemon, target, move) {`
    if line.contains('(') && line.contains(')') && line.ends_with('{') {
        let paren_pos = line.find('(').unwrap();
        let before = &line[..paren_pos];
        // Must start with a word character (function name)
        if before.chars().all(|c| c.is_alphanumeric() || c == '_') && !before.is_empty() {
            return true;
        }
    }
    false
}

/// Collect lines for a nested block starting at `start_idx`, returning (block_text, end_idx).
/// The line at `start_idx` should contain the opening `{`.
fn collect_block(lines: &[String], start_idx: usize) -> (String, usize) {
    let mut depth: i32 = 0;
    let mut block = String::new();
    for i in start_idx..lines.len() {
        let line = &lines[i];
        depth += line.chars().filter(|&c| c == '{').count() as i32;
        depth -= line.chars().filter(|&c| c == '}').count() as i32;
        block.push_str(line);
        block.push(' ');
        if depth <= 0 {
            return (block, i);
        }
    }
    (block, lines.len().saturating_sub(1))
}

/// Parse a secondary effect block.
/// Returns (target_secondary, self_secondary, end_idx).
/// The `self: { ... }` sub-block is extracted and returned as a separate optional secondary.
fn parse_secondary_block(
    lines: &[String],
    start_idx: usize,
) -> (
    Option<PokemonSecondaryEffect>,
    Option<PokemonSecondaryEffect>,
    usize,
) {
    let (block_text, end_idx) = collect_block(lines, start_idx);

    // Parse chance
    let chance: u8 = extract_int(&block_text, "chance").unwrap_or(0);

    // Separate self: { ... } from the rest
    let (target_text, self_text) = extract_self_subblock(&block_text);

    // Parse target effects
    let target_effect = parse_effect_from_text(&target_text);
    // Random `this.sample([...])` choices inside an onHit (e.g. Tri Attack, Dire Claw).
    let target_random = parse_sample_effects(&target_text);
    let has_target = chance > 0
        || target_effect.status.is_some()
        || target_effect.volatile_status.is_some()
        || target_effect.side_condition.is_some()
        || target_effect.terrain.is_some()
        || target_effect.weather.is_some()
        || target_effect.pseudo_weather.is_some()
        || target_effect.slot_condition.is_some()
        || target_effect.boosts.iter().any(|&b| b != 0)
        || !target_random.is_empty();
    let target_sec = if has_target {
        Some(PokemonSecondaryEffect {
            chance,
            effect: target_effect,
            random_choices: target_random,
        })
    } else {
        None
    };

    // Parse self effects
    let self_sec = if let Some(st) = self_text {
        let self_effect = parse_effect_from_text(&st);
        let self_random = parse_sample_effects(&st);
        let has_self = self_effect.status.is_some()
            || self_effect.volatile_status.is_some()
            || self_effect.side_condition.is_some()
            || self_effect.terrain.is_some()
            || self_effect.weather.is_some()
            || self_effect.pseudo_weather.is_some()
            || self_effect.slot_condition.is_some()
            || self_effect.boosts.iter().any(|&b| b != 0)
            || !self_random.is_empty();
        if has_self {
            Some(PokemonSecondaryEffect {
                chance,
                effect: self_effect,
                random_choices: self_random,
            })
        } else {
            None
        }
    } else {
        None
    };

    (target_sec, self_sec, end_idx)
}

// --- Public Dex Parsing ---

/// Parse one entry from the Pokémon dex lines into a `(species, PokemonData)` pair.
fn parse_pokemon_entry(lines: &[String]) -> Option<(Species, PokemonData)> {
    let mut species: Option<Species> = None;
    let mut types: Vec<PokemonType> = Vec::new();
    let mut base_stats = [0u16; 6];
    let mut weight: u16 = 0;
    let mut primary_ability: Option<Ability> = None;
    let mut base_species: Option<Species> = None;
    let mut forme: Option<Species> = None;
    let mut required_item: Option<String> = None;
    let mut battle_only: Option<Species> = None;
    let mut default_gender = crate::state::pokemon::PokemonGender::Male;
    let mut has_explicit_gender = false;

    for line in lines {
        let trimmed = line.trim();

        if trimmed.starts_with("name:") {
            if let Some(name) = extract_quoted(trimmed, "name") {
                species = Some(Species::from_str(&name));
            }
        } else if trimmed.starts_with("types:") {
            // types: ["Grass", "Poison"],
            if let Some(start) = trimmed.find('[') {
                if let Some(end) = trimmed.find(']') {
                    let inner = &trimmed[start + 1..end];
                    for part in inner.split(',') {
                        let t = part.trim().trim_matches('"');
                        if let Some(pt) = parse_type(t) {
                            types.push(pt);
                        }
                    }
                }
            }
        } else if trimmed.starts_with("baseStats:") {
            // baseStats: { hp: 45, atk: 49, def: 49, spa: 65, spd: 65, spe: 45 },
            if let Some(start) = trimmed.find('{') {
                if let Some(end) = trimmed.find('}') {
                    let inner = &trimmed[start + 1..end];
                    for part in inner.split(',') {
                        if let Some((k, v)) = part.split_once(':') {
                            let k = k.trim();
                            let v: u16 = v.trim().parse().unwrap_or(0);
                            match k {
                                "hp" => base_stats[0] = v,
                                "atk" => base_stats[1] = v,
                                "def" => base_stats[2] = v,
                                "spa" => base_stats[3] = v,
                                "spd" => base_stats[4] = v,
                                "spe" => base_stats[5] = v,
                                _ => {}
                            }
                        }
                    }
                }
            }
        } else if trimmed.starts_with("weightkg:") {
            // weightkg: 6.9, -> store as hectograms (u16)
            if let Some(rest) = trimmed.strip_prefix("weightkg:") {
                let val_str: String = rest
                    .trim()
                    .trim_end_matches(',')
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.')
                    .collect();
                if let Ok(w) = val_str.parse::<f64>() {
                    weight = (w * 10.0).round() as u16; // hectograms
                }
            }
        } else if trimmed.starts_with("abilities:") {
            primary_ability = parse_primary_ability_from_text(trimmed);
        } else if trimmed.starts_with("baseSpecies:") {
            if let Some(val) = extract_quoted(trimmed, "baseSpecies") {
                base_species = Some(Species::from_str(&val));
            }
        } else if trimmed.starts_with("forme:") {
            if let Some(val) = extract_quoted(trimmed, "forme") {
                forme = Some(Species::from_str(&val));
            }
        } else if trimmed.starts_with("requiredItem:") {
            if let Some(val) = extract_quoted(trimmed, "requiredItem") {
                required_item = Some(normalize_dex_id(&val));
            }
        } else if trimmed.starts_with("battleOnly:") {
            // Handles both `battleOnly: "X"` and `battleOnly: ["X", ...]`.
            if let Some(val) = extract_first_quoted_value(trimmed) {
                battle_only = Some(Species::from_str(&val));
            }
        } else if trimmed.starts_with("gender:") {
            if let Some(val) = extract_quoted(trimmed, "gender") {
                has_explicit_gender = true;
                if val == "M" {
                    default_gender = crate::state::pokemon::PokemonGender::Male;
                } else if val == "F" {
                    default_gender = crate::state::pokemon::PokemonGender::Female;
                } else if val == "N" {
                    default_gender = crate::state::pokemon::PokemonGender::Genderless;
                }
            }
        } else if trimmed.starts_with("genderRatio:") && !has_explicit_gender {
            // genderRatio: { M: 0.875, F: 0.125 }
            let mut m_ratio = 0.5;
            let mut f_ratio = 0.5;
            if let Some(start) = trimmed.find('{') {
                if let Some(end) = trimmed.find('}') {
                    let inner = &trimmed[start + 1..end];
                    for part in inner.split(',') {
                        if let Some((k, v)) = part.split_once(':') {
                            let k = k.trim();
                            if let Ok(val) = v.trim().parse::<f64>() {
                                if k == "M" {
                                    m_ratio = val;
                                } else if k == "F" {
                                    f_ratio = val;
                                }
                            }
                        }
                    }
                }
            }
            if m_ratio > f_ratio {
                default_gender = crate::state::pokemon::PokemonGender::Male;
            } else if f_ratio > m_ratio {
                default_gender = crate::state::pokemon::PokemonGender::Female;
            } else {
                // Default to Male if equal
                default_gender = crate::state::pokemon::PokemonGender::Male;
            }
        }
    }

    let s = species?;
    Some((
        s.clone(),
        PokemonData {
            species: s,
            types,
            base_stats,
            weight,
            primary_ability,
            base_species,
            forme,
            required_item,
            battle_only,
            default_gender,
        },
    ))
}

/// Parse showdownDex.txt into a HashMap of PokemonData keyed by species id.
pub fn parse_pokemon_dex(file_path: &str) -> HashMap<Species, PokemonData> {
    let content = fs::read_to_string(file_path).expect("Failed to read Pokemon dex file");
    let entries = split_entries(&content);
    let mut result = HashMap::new();
    for (_key, lines) in &entries {
        if let Some((species, data)) = parse_pokemon_entry(lines) {
            result.insert(species, data);
        }
    }
    result
}

/// Parse a single entry (slice of lines) from the move dex into a `(move, MoveData)` pair.
fn parse_move_entry(lines: &[String]) -> Option<(PokemonMove, MoveData)> {
    let mut name: Option<PokemonMove> = None;
    let mut accuracy = AccuracyType::Percent(100);
    let mut pp: u8 = 0;
    let mut category = MoveCategory::Status;
    let mut pokemon_type = PokemonType::Normal;
    let mut priority: i8 = 0;
    let mut target = MoveTarget::Normal;
    let mut flags: Vec<MoveFlag> = Vec::new();
    let mut base_power: u16 = 0;

    let mut ohko = false;
    let mut thaws_target = false;
    let mut heal_fraction: [u8; 2] = [0, 0];
    let mut force_switch = false;
    let mut self_switch = SelfSwitchType::None;
    let mut self_boost = [0i8; 7];
    let mut top_level_boosts: Option<[i8; 7]> = None;
    let mut self_destruct = SelfDestructType::None;
    let mut breaks_protect = false;
    let mut recoil_fraction: [u8; 2] = [0, 0];
    let mut drain_fraction: [u8; 2] = [0, 0];
    let mut mind_blown_recoil = false;
    let mut struggle_recoil = false;

    let mut secondaries: Vec<PokemonSecondaryEffect> = Vec::new();
    let mut self_secondaries: Vec<PokemonSecondaryEffect> = Vec::new();

    let mut crit_ratio: u8 = 1;
    let mut foul_play = false;

    let mut ignore_ability = false;
    let mut ignore_defense_boosts = false;
    let mut ignore_evasion = false;
    let mut ignore_immunity: Vec<PokemonType> = Vec::new();

    let mut multihit_range: [u8; 2] = [0, 0];
    let mut multihit_accuracy = false;

    let mut sleep_usable = false;
    let mut has_crash_damage = false;
    let mut stalling_move = false;
    let mut override_offensive_stat: Option<PokemonStat> = None;
    let mut override_defensive_stat: Option<PokemonStat> = None;

    let mut i = 0;
    let mut depth: i32 = 0;
    let mut skip_until_depth: Option<i32> = None;
    let mut damage_override: DamageOverride = DamageOverride::None;

    while i < lines.len() {
        let line = &lines[i];
        let trimmed = line.trim();

        // Track depth from nested braces
        let open = trimmed.chars().filter(|&c| c == '{').count() as i32;
        let close = trimmed.chars().filter(|&c| c == '}').count() as i32;

        // Skip function bodies
        if is_function_line(trimmed) {
            let restore_depth = depth;
            depth += open - close;
            i += 1;
            // Skip until we return to restore_depth
            while i < lines.len() {
                let l = &lines[i];
                depth += l.chars().filter(|&c| c == '{').count() as i32;
                depth -= l.chars().filter(|&c| c == '}').count() as i32;
                i += 1;
                if depth <= restore_depth {
                    break;
                }
            }
            continue;
        }

        // If we are skipping a nested block, track depth and skip
        if let Some(target_depth) = skip_until_depth {
            depth += open - close;
            if depth <= target_depth {
                skip_until_depth = None;
            }
            i += 1;
            continue;
        }

        depth += open - close;

        // Only parse fields at depth 0 (relative within the entry)
        if depth > 1 {
            i += 1;
            continue;
        }

        // -- Simple fields --
        if trimmed.starts_with("name:") {
            if let Some(n) = extract_quoted(trimmed, "name") {
                name = Some(PokemonMove::from_str(&n));
            }
        } else if trimmed.starts_with("accuracy:") {
            if extract_bool(trimmed, "accuracy") == Some(true) {
                accuracy = AccuracyType::True;
            } else if let Some(v) = extract_int::<u8>(trimmed, "accuracy") {
                accuracy = AccuracyType::Percent(v);
            }
        } else if trimmed.starts_with("pp:") {
            if let Some(v) = extract_int::<u8>(trimmed, "pp") {
                pp = v;
            }
        } else if trimmed.starts_with("basePower:") {
            if let Some(v) = extract_int::<u16>(trimmed, "basePower") {
                base_power = v;
            }
        } else if trimmed.starts_with("category:") {
            if let Some(c) = extract_quoted(trimmed, "category") {
                category = parse_category(&c);
            }
        } else if trimmed.starts_with("type:") {
            if let Some(t) = extract_quoted(trimmed, "type") {
                if let Some(pt) = parse_type(&t) {
                    pokemon_type = pt;
                }
            }
        } else if trimmed.starts_with("priority:") {
            if let Some(v) = extract_int::<i8>(trimmed, "priority") {
                priority = v;
            }
        } else if trimmed.starts_with("target:") {
            if let Some(t) = extract_quoted(trimmed, "target") {
                target = parse_target(&t);
            }
        } else if trimmed.starts_with("flags:") {
            // flags might be on one line: flags: { contact: 1, protect: 1 },
            // or might span multiple lines
            if trimmed.contains('}') {
                flags = parse_flags_from_text(trimmed);
            } else if trimmed.contains('{') {
                let (block, end) = collect_block(lines, i);
                flags = parse_flags_from_text(&block);
                i = end;
            }
        } else if trimmed.starts_with("critRatio:") {
            if let Some(v) = extract_int::<u8>(trimmed, "critRatio") {
                crit_ratio = v;
            }
        } else if trimmed.starts_with("drain:") {
            if let Some(arr) = extract_array2(trimmed, "drain") {
                drain_fraction = arr;
            }
        } else if trimmed.starts_with("recoil:") {
            if let Some(arr) = extract_array2(trimmed, "recoil") {
                recoil_fraction = arr;
            }
        } else if trimmed.starts_with("heal:") {
            if let Some(arr) = extract_array2(trimmed, "heal") {
                heal_fraction = arr;
            }
        } else if trimmed.starts_with("multihit:") {
            if let Some(arr) = extract_array2(trimmed, "multihit") {
                multihit_range = arr;
            } else if let Some(v) = extract_int::<u8>(trimmed, "multihit") {
                multihit_range = [v, v];
            }
        } else if trimmed.starts_with("multiaccuracy:") {
            multihit_accuracy = true;
        } else if trimmed.starts_with("ohko:") {
            ohko = true;
        } else if trimmed.starts_with("thawsTarget:") {
            thaws_target = extract_bool(trimmed, "thawsTarget").unwrap_or(false);
        } else if trimmed.starts_with("forceSwitch:") {
            force_switch = extract_bool(trimmed, "forceSwitch").unwrap_or(false);
        } else if trimmed.starts_with("selfSwitch:") {
            if extract_bool(trimmed, "selfSwitch") == Some(true) {
                self_switch = SelfSwitchType::Normal;
            } else if let Some(s) = extract_quoted(trimmed, "selfSwitch") {
                self_switch = match s.as_str() {
                    "shedtail" => SelfSwitchType::ShedTail,
                    "copyvolatile" => SelfSwitchType::BatonPass,
                    _ => SelfSwitchType::Normal,
                };
            }
        } else if trimmed.starts_with("selfdestruct:") {
            if let Some(s) = extract_quoted(trimmed, "selfdestruct") {
                self_destruct = match s.as_str() {
                    "always" => SelfDestructType::Always,
                    "ifHit" => SelfDestructType::IfHit,
                    _ => SelfDestructType::None,
                };
            }
        } else if trimmed.starts_with("breaksProtect:") {
            breaks_protect = extract_bool(trimmed, "breaksProtect").unwrap_or(false);
        } else if trimmed.starts_with("mindBlownRecoil:") {
            mind_blown_recoil = extract_bool(trimmed, "mindBlownRecoil").unwrap_or(false);
        } else if trimmed.starts_with("struggleRecoil:") {
            struggle_recoil = extract_bool(trimmed, "struggleRecoil").unwrap_or(false);
        } else if trimmed.starts_with("ignoreAbility:") {
            ignore_ability = extract_bool(trimmed, "ignoreAbility").unwrap_or(false);
        } else if trimmed.starts_with("ignoreDefensive:") {
            ignore_defense_boosts = extract_bool(trimmed, "ignoreDefensive").unwrap_or(false);
        } else if trimmed.starts_with("ignoreEvasion:") {
            ignore_evasion = extract_bool(trimmed, "ignoreEvasion").unwrap_or(false);
        } else if trimmed.starts_with("ignoreImmunity:") {
            if extract_bool(trimmed, "ignoreImmunity") == Some(true) {
                // Ignores all type immunities
                ignore_immunity = vec![
                    PokemonType::Normal,
                    PokemonType::Fire,
                    PokemonType::Water,
                    PokemonType::Electric,
                    PokemonType::Grass,
                    PokemonType::Ice,
                    PokemonType::Fighting,
                    PokemonType::Poison,
                    PokemonType::Ground,
                    PokemonType::Flying,
                    PokemonType::Psychic,
                    PokemonType::Bug,
                    PokemonType::Rock,
                    PokemonType::Ghost,
                    PokemonType::Dragon,
                    PokemonType::Dark,
                    PokemonType::Steel,
                    PokemonType::Fairy,
                ];
            } else if trimmed.contains('{') && trimmed.contains('}') {
                // ignoreImmunity: { 'Fairy': true }
                let inner = &trimmed[trimmed.find('{').unwrap()..=trimmed.rfind('}').unwrap()];
                for part in inner.split(',') {
                    let part = part
                        .trim()
                        .trim_matches(|c: char| c == '{' || c == '}' || c == '\'' || c == '"');
                    if let Some((type_name, _)) = part.split_once(':') {
                        let type_name = type_name
                            .trim()
                            .trim_matches(|c: char| c == '\'' || c == '"');
                        if let Some(pt) = parse_type(type_name) {
                            ignore_immunity.push(pt);
                        }
                    }
                }
            }
        } else if trimmed.starts_with("sleepUsable:") {
            sleep_usable = extract_bool(trimmed, "sleepUsable").unwrap_or(false);
        } else if trimmed.starts_with("hasCrashDamage:") {
            has_crash_damage = extract_bool(trimmed, "hasCrashDamage").unwrap_or(false);
        } else if trimmed.starts_with("overrideOffensivePokemon:") {
            if let Some(s) = extract_quoted(trimmed, "overrideOffensivePokemon") {
                if s == "target" {
                    foul_play = true;
                }
            }
        } else if trimmed.starts_with("boosts:") && depth <= 1 {
            // Top-level boosts — must extract only the { ... } inner content,
            // not the "boosts: " prefix, or parse_boosts_from_text mis-splits it.
            if trimmed.contains('{') && trimmed.contains('}') {
                if let (Some(ob), Some(cb)) = (trimmed.find('{'), trimmed.rfind('}')) {
                    top_level_boosts = Some(parse_boosts_from_text(&trimmed[ob..=cb]));
                }
            } else if trimmed.contains('{') {
                let (block, end) = collect_block(lines, i);
                if let (Some(ob), Some(cb)) = (block.find('{'), block.rfind('}')) {
                    top_level_boosts = Some(parse_boosts_from_text(&block[ob..=cb]));
                }
                i = end;
            }
        } else if trimmed.starts_with("secondary:") {
            if trimmed.contains("null") {
                // No secondary
            } else if trimmed.contains('{') {
                let (target_sec, self_sec, end) = parse_secondary_block(lines, i);
                if let Some(s) = target_sec {
                    secondaries.push(s);
                }
                if let Some(s) = self_sec {
                    self_secondaries.push(s);
                }
                i = end;
            }
        } else if trimmed.starts_with("secondaries:") {
            if trimmed.contains('[') {
                let (_block, end) = collect_block(lines, i);
                let block_lines: Vec<String> =
                    lines[i..=end].iter().map(|s| s.to_string()).collect();
                let mut k = 0;
                while k < block_lines.len() {
                    let bl = block_lines[k].trim();
                    if bl.starts_with('{') || bl.contains("chance:") {
                        let (target_sec, self_sec, sec_end) =
                            parse_secondary_block(&block_lines, k);
                        if let Some(s) = target_sec {
                            secondaries.push(s);
                        }
                        if let Some(s) = self_sec {
                            self_secondaries.push(s);
                        }
                        k = sec_end + 1;
                        continue;
                    }
                    k += 1;
                }
                i = end;
            }
        } else if trimmed.starts_with("status:") && depth <= 1 {
            // Top-level status: always-apply 100% secondary
            if let Some(s) = extract_quoted(trimmed, "status") {
                if let Some(nv) = parse_nvstatus(&s) {
                    let mut e = empty_hit_effect();
                    e.status = Some(nv);
                    secondaries.push(PokemonSecondaryEffect::simple(100, e));
                }
            }
        } else if trimmed.starts_with("volatileStatus:") && depth <= 1 {
            // Top-level volatileStatus: always-apply 100% secondary
            if let Some(s) = extract_quoted(trimmed, "volatileStatus") {
                if let Some(vs) = parse_volatile(&s) {
                    let mut e = empty_hit_effect();
                    e.volatile_status = Some(vs);
                    secondaries.push(PokemonSecondaryEffect::simple(100, e));
                }
            }
        } else if trimmed.starts_with("sideCondition:") && depth <= 1 {
            if let Some(s) = extract_quoted(trimmed, "sideCondition") {
                if let Some(sc) = parse_side_condition(&s) {
                    let mut e = empty_hit_effect();
                    e.side_condition = Some(sc);
                    secondaries.push(PokemonSecondaryEffect::simple(100, e));
                }
            }
        } else if trimmed.starts_with("terrain:") && depth <= 1 {
            if let Some(s) = extract_quoted(trimmed, "terrain") {
                if let Some(t) = parse_terrain(&s) {
                    let mut e = empty_hit_effect();
                    e.terrain = Some(t);
                    secondaries.push(PokemonSecondaryEffect::simple(100, e));
                }
            }
        } else if trimmed.starts_with("weather:") && depth <= 1 {
            if let Some(s) = extract_quoted(trimmed, "weather") {
                if let Some(w) = parse_weather_val(&s) {
                    let mut e = empty_hit_effect();
                    e.weather = Some(w);
                    secondaries.push(PokemonSecondaryEffect::simple(100, e));
                }
            }
        } else if trimmed.starts_with("pseudoWeather:") && depth <= 1 {
            if let Some(s) = extract_quoted(trimmed, "pseudoWeather") {
                if let Some(pw) = parse_pseudo_weather(&s) {
                    let mut e = empty_hit_effect();
                    e.pseudo_weather = Some(pw);
                    secondaries.push(PokemonSecondaryEffect::simple(100, e));
                }
            }
        } else if trimmed.starts_with("slotCondition:") && depth <= 1 {
            if let Some(s) = extract_quoted(trimmed, "slotCondition") {
                if let Some(slc) = parse_slot_condition(&s) {
                    let mut e = empty_hit_effect();
                    e.slot_condition = Some(slc);
                    secondaries.push(PokemonSecondaryEffect::simple(100, e));
                }
            }
        } else if trimmed.starts_with("self:")
            && !trimmed.starts_with("selfSwitch")
            && !trimmed.starts_with("selfdestruct")
            && depth <= 1
        {
            // Top-level self: { ... } block — always-apply 100% self-secondary
            let (block, end) = collect_block(lines, i);
            if let Some(ob) = block.find('{') {
                if let Some(cb) = block.rfind('}') {
                    if cb > ob {
                        let inner = block[ob + 1..cb].to_string();
                        let self_effect = parse_effect_from_text(&inner);
                        let has_self = self_effect.status.is_some()
                            || self_effect.volatile_status.is_some()
                            || self_effect.boosts.iter().any(|&b| b != 0);
                        if has_self {
                            self_secondaries.push(PokemonSecondaryEffect::simple(100, self_effect));
                        }
                    }
                }
            }
            i = end;
        } else if trimmed.starts_with("willCrit:") {
            if extract_bool(trimmed, "willCrit") == Some(true) {
                crit_ratio = 6; // guaranteed critical hit
            }
        } else if trimmed.starts_with("stallingMove:") {
            stalling_move = extract_bool(trimmed, "stallingMove").unwrap_or(false);
        } else if trimmed.starts_with("overrideOffensiveStat:") {
            if let Some(s) = extract_quoted(trimmed, "overrideOffensiveStat") {
                override_offensive_stat = parse_stat(&s);
            }
        } else if trimmed.starts_with("overrideDefensiveStat:") {
            if let Some(s) = extract_quoted(trimmed, "overrideDefensiveStat") {
                override_defensive_stat = parse_stat(&s);
            }
        } else if trimmed.starts_with("selfBoost:") {
            let (block, end) = collect_block(lines, i);
            if let Some(bp) = block.find("boosts:") {
                let rest = &block[bp..];
                if let Some(ob) = rest.find('{') {
                    let inner = &rest[ob..];
                    if let Some(cb) = inner.rfind('}') {
                        self_boost = parse_boosts_from_text(&inner[..=cb]);
                    }
                }
            }
            i = end;
        } else if trimmed.starts_with("damage:") && depth <= 1 {
            if let Some(s) = extract_quoted(trimmed, "damage") {
                damage_override = parse_damage_override(&s).unwrap_or(DamageOverride::None);
            } else if let Some(v) = extract_int::<u16>(trimmed, "damage") {
                damage_override = DamageOverride::Number(v);
            }
        } else if trimmed.starts_with("damageOverride:") {
            if let Some(s) = extract_quoted(trimmed, "damageOverride") {
                damage_override = parse_damage_override(&s).unwrap_or(DamageOverride::None);
            }
        }

        i += 1;
    }

    // Decide where top-level boosts belong based on move target
    if let Some(boosts) = top_level_boosts {
        let self_targeting = matches!(
            target,
            MoveTarget::SelfTarget | MoveTarget::AllySide | MoveTarget::AllyTeam
        );
        if self_targeting {
            self_boost = boosts;
        } else {
            let mut e = empty_hit_effect();
            e.boosts = boosts;
            secondaries.push(PokemonSecondaryEffect::simple(100, e));
        }
    }

    // Ceaseless Edge and Stone Axe set an entry hazard on hit via JS code the parser cannot read.
    // Inject the equivalent always-on foe-side secondary so they flow through the normal
    // side-condition pipeline (and stack with existing layers / record the Sticky Web setter).
    match &name {
        Some(PokemonMove::CeaselessEdge) => {
            let mut e = empty_hit_effect();
            e.side_condition = Some(SideCondition::Spikes(1));
            secondaries.push(PokemonSecondaryEffect::simple(100, e));
        }
        Some(PokemonMove::StoneAxe) => {
            let mut e = empty_hit_effect();
            e.side_condition = Some(SideCondition::StealthRock);
            secondaries.push(PokemonSecondaryEffect::simple(100, e));
        }
        _ => {}
    }

    let n = name?;
    Some((
        n.clone(),
        MoveData {
            name: n,
            accuracy,
            pp,
            category,
            pokemon_type,
            priority,
            target,
            base_power,
            flags,
            ohko,
            thaws_target,
            heal_fraction,
            force_switch,
            self_switch,
            self_boost,
            self_destruct,
            breaks_protect,
            recoil_fraction,
            drain_fraction,
            mind_blown_recoil,
            struggle_recoil,
            secondaries,
            self_secondaries,
            crit_ratio,
            foul_play,
            ignore_ability,
            ignore_defense_boosts,
            ignore_evasion,
            ignore_immunity,
            multihit_range,
            multihit_accuracy,
            sleep_usable,
            has_crash_damage,
            damage_override,
            stalling_move,
            override_offensive_stat,
            override_defensive_stat,
        },
    ))
}

/// Parse showdownMoves.txt into a HashMap of MoveData keyed by move id.
pub fn parse_move_dex(file_path: &str) -> HashMap<PokemonMove, MoveData> {
    let content = fs::read_to_string(file_path).expect("Failed to read moves file");
    let entries = split_entries(&content);
    let mut result = HashMap::new();
    for (_key, lines) in &entries {
        if let Some((name, data)) = parse_move_entry(lines) {
            result.insert(name, data);
        }
    }
    result
}
