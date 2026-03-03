use std::collections::HashMap;
use std::fs;

pub type PokemonBoostArr = [i8; 7]; // atk, def, spa, spd, spe, accuracy, evasion

#[derive(Debug, Clone)]
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
pub enum PokemonAccuracy {
    True,
    Percent(u8),
}

#[derive(Debug)]
pub enum PokemonMoveCategory {
    Physical,
    Special,
    Status,
}

#[derive(Debug)]
pub enum PokemonMoveTarget {
    AdjacentAlly,
    AdjacentAllyOrSelf,
    AdjacentFoe,
    All,
    AllAdjacent,
    AllAdjacentFoes,
    Allies,
    AllySide,
    AllyTeam,
    Any,
    FoeSide,
    Normal,
    RandomNormal,
    Scripted,
    SelfTarget,
}

#[derive(Debug, Clone)]
pub enum PokemonMoveFlag {
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
pub enum PokemonDamageOverride {
    Number(u16),
    Level,
    None,
}

#[derive(Debug, Clone, Copy)]
pub enum PokemonStat {
    Atk,
    Def,
    SpA,
    SpD,
    Spe,
}

#[derive(Debug)]
pub enum PokemonSelfSwitchType {
    ShedTail,
    BatonPass,
    Normal,
    None,
}

#[derive(Debug)]
pub enum PokemonSelfDestructType {
    Always,
    IfHit,
    None,
}

#[derive(Debug, Clone)]
pub enum PokemonNonVolatileStatusType {
    Burn,
    Poison,
    ToxicPoison,
    Paralysis,
    Sleep,
    Frozen,
}

#[derive(Debug, Clone)]
pub enum PokemonVolatileStatusType {
    Flinch,
    AquaRing,
    Attract,
    Confusion,
    BanefulBunker,
    Bide,
    PartiallyTrapped,
    MustRecharge,
    BurningBulwark,
    Charge,
    Curse,
    DefenseCurl,
    DestinyBond,
    Protect,
    Disable,
    DragonCheer,
    Electrify,
    Embargo,
    Encore,
    Endure,
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
    LockedMove,
    Powder,
    PowerShift,
    PowerTrick,
    Rage,
    RagePowder,
    Roost,
    SaltCure,
    Substitute,
    SilkTrap,
    SmackDown,
    Snatch,
    SparklingAria,
    SpikyShield,
    Spotlight,
    Stockpile,
    SyrupBomb,
    TarShot,
    Taunt,
    Telekinesis,
    Torment,
    Uproar,
    Yawn,
}

#[derive(Debug)]
pub enum PokemonSideCondition {
    AuroraVeil,
    Reflect,
    CraftyShield,
    LightScreen,
    LuckyChant,
    MatBlock,
    Mist,
    QuickGuard,
    SafeGuard,
    Spikes,
    StealthRock,
    StickyWeb,
    TailWind,
    ToxicSpikes,
    WideGuard,
}

#[derive(Debug, Clone)]
pub enum PokemonSlotCondition {
    FutureMove,
    HealingWish,
    LunarDance,
    RevivalBlessing,
    Wish,
}

#[derive(Debug, Clone)]
pub enum PokemonPseudoWeather {
    FairyLock,
    Gravity,
    IonDeluge,
    MagicDeluge,
    MudSport,
    WaterSport,
    WonderRoom,
}

#[derive(Debug, Clone)]
pub enum PokemonTerrain {
    ElectricTerrain,
    GrassyTerrain,
    MistyTerrain,
    PsychicTerrain,
}

#[derive(Debug, Clone)]
pub enum PokemonWeather {
    Rain,
    Sandstorm,
    Snow,
    Sun,
}

#[derive(Debug)]
pub struct PokemonHitEffect {
    pub boosts: PokemonBoostArr,
    pub status: Option<PokemonNonVolatileStatusType>,
    pub volatile_status: Option<PokemonVolatileStatusType>,
    pub slot_condition: Option<PokemonSlotCondition>,
    pub side_condition: Option<PokemonSideCondition>,
    pub pseudo_weather: Option<PokemonPseudoWeather>,
    pub terrain: Option<PokemonTerrain>,
    pub weather: Option<PokemonWeather>,
}

#[derive(Debug)]
pub struct PokemonSecondaryEffect {
    pub chance: u8,
    pub effect: PokemonHitEffect,
}

#[derive(Debug)]
pub struct PokemonMoveData {
    pub name: String,
    pub base_power: u16,
    pub accuracy: PokemonAccuracy,
    pub target: PokemonMoveTarget,
    pub secondaries: Vec<PokemonSecondaryEffect>,
    pub self_secondaries: Vec<PokemonSecondaryEffect>,
    pub pp: u8,

    pub category: PokemonMoveCategory,
    pub pokemon_type: PokemonType,
    pub priority: i8,
    pub flags: Vec<PokemonMoveFlag>,

    // Hit Effects
    pub ohko: bool,
    pub thaws_target: bool,
    pub heal_fraction: [u8; 2],
    pub force_switch: bool,
    pub self_switch: PokemonSelfSwitchType,
    pub self_boost: PokemonBoostArr,
    pub self_destruct: PokemonSelfDestructType,
    pub breaks_protect: bool,
    pub recoil_fraction: [u8; 2],
    pub drain_fraction: [u8; 2],
    pub mind_blown_recoil: bool,
    pub struggle_recoil: bool,
    pub steals_boosts: bool,

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
    pub smart_target: bool,
    pub tracks_target: bool,
    pub calls_move: bool,

    pub has_crash_damage: bool,
    pub damage_override: PokemonDamageOverride,

    pub stalling_move: bool,
    pub override_offensive_stat: Option<PokemonStat>,
    pub override_defensive_stat: Option<PokemonStat>,
}

#[derive(Debug)]
pub struct PokemonData {
    pub species: String,
    pub types: Vec<PokemonType>,
    pub base_stats: [u16; 6],
    pub weight: u16,
}

// --- Helpers ---

fn parse_type(s: &str) -> Option<PokemonType> {
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

fn parse_target(s: &str) -> PokemonMoveTarget {
    match s {
        "adjacentAlly" => PokemonMoveTarget::AdjacentAlly,
        "adjacentAllyOrSelf" => PokemonMoveTarget::AdjacentAllyOrSelf,
        "adjacentFoe" => PokemonMoveTarget::AdjacentFoe,
        "all" => PokemonMoveTarget::All,
        "allAdjacent" => PokemonMoveTarget::AllAdjacent,
        "allAdjacentFoes" => PokemonMoveTarget::AllAdjacentFoes,
        "allies" => PokemonMoveTarget::Allies,
        "allySide" => PokemonMoveTarget::AllySide,
        "allyTeam" => PokemonMoveTarget::AllyTeam,
        "any" => PokemonMoveTarget::Any,
        "foeSide" => PokemonMoveTarget::FoeSide,
        "normal" => PokemonMoveTarget::Normal,
        "randomNormal" => PokemonMoveTarget::RandomNormal,
        "scripted" => PokemonMoveTarget::Scripted,
        "self" => PokemonMoveTarget::SelfTarget,
        _ => PokemonMoveTarget::Normal,
    }
}

fn parse_category(s: &str) -> PokemonMoveCategory {
    match s {
        "Physical" => PokemonMoveCategory::Physical,
        "Special" => PokemonMoveCategory::Special,
        _ => PokemonMoveCategory::Status,
    }
}

fn parse_flag(s: &str) -> Option<PokemonMoveFlag> {
    match s {
        "bypasssub" => Some(PokemonMoveFlag::BypassSub),
        "bite" => Some(PokemonMoveFlag::Bite),
        "bullet" => Some(PokemonMoveFlag::Bullet),
        "cantusetwice" => Some(PokemonMoveFlag::CantUseTwice),
        "charge" => Some(PokemonMoveFlag::Charge),
        "contact" => Some(PokemonMoveFlag::Contact),
        "dance" => Some(PokemonMoveFlag::Dance),
        "defrost" => Some(PokemonMoveFlag::Defrost),
        "distance" => Some(PokemonMoveFlag::Distance),
        "failcopycat" => Some(PokemonMoveFlag::FailCopyCat),
        "failencore" => Some(PokemonMoveFlag::FailEncore),
        "failinstruct" => Some(PokemonMoveFlag::FailInstruct),
        "failmefirst" => Some(PokemonMoveFlag::FailMeFirst),
        "failmimic" => Some(PokemonMoveFlag::FailMimic),
        "futuremove" => Some(PokemonMoveFlag::FutureMove),
        "gravity" => Some(PokemonMoveFlag::Gravity),
        "heal" => Some(PokemonMoveFlag::Heal),
        "metronome" => Some(PokemonMoveFlag::Metronome),
        "mirror" => Some(PokemonMoveFlag::Mirror),
        "mustpressure" => Some(PokemonMoveFlag::MustPressure),
        "noassist" => Some(PokemonMoveFlag::NoAssist),
        "noparentalbond" => Some(PokemonMoveFlag::NoParentalBond),
        "nosketch" => Some(PokemonMoveFlag::NoSketch),
        "nosleeptalk" => Some(PokemonMoveFlag::NoSleepTalk),
        "pledgecombo" => Some(PokemonMoveFlag::PledgeCombo),
        "powder" => Some(PokemonMoveFlag::Powder),
        "protect" => Some(PokemonMoveFlag::Protect),
        "pulse" => Some(PokemonMoveFlag::Pulse),
        "punch" => Some(PokemonMoveFlag::Punch),
        "recharge" => Some(PokemonMoveFlag::Recharge),
        "reflectable" => Some(PokemonMoveFlag::Reflectable),
        "slicing" => Some(PokemonMoveFlag::Slicing),
        "snatch" => Some(PokemonMoveFlag::Snatch),
        "sound" => Some(PokemonMoveFlag::Sound),
        "wind" => Some(PokemonMoveFlag::Wind),
        _ => None,
    }
}

fn parse_nvstatus(s: &str) -> Option<PokemonNonVolatileStatusType> {
    match s {
        "brn" => Some(PokemonNonVolatileStatusType::Burn),
        "psn" => Some(PokemonNonVolatileStatusType::Poison),
        "tox" => Some(PokemonNonVolatileStatusType::ToxicPoison),
        "par" => Some(PokemonNonVolatileStatusType::Paralysis),
        "slp" => Some(PokemonNonVolatileStatusType::Sleep),
        "frz" => Some(PokemonNonVolatileStatusType::Frozen),
        _ => None,
    }
}

fn parse_volatile(s: &str) -> Option<PokemonVolatileStatusType> {
    match s {
        "flinch" => Some(PokemonVolatileStatusType::Flinch),
        "aquaring" => Some(PokemonVolatileStatusType::AquaRing),
        "attract" => Some(PokemonVolatileStatusType::Attract),
        "confusion" => Some(PokemonVolatileStatusType::Confusion),
        "banefulbunker" => Some(PokemonVolatileStatusType::BanefulBunker),
        "bide" => Some(PokemonVolatileStatusType::Bide),
        "partiallytrapped" => Some(PokemonVolatileStatusType::PartiallyTrapped),
        "mustrecharge" => Some(PokemonVolatileStatusType::MustRecharge),
        "burningbulwark" => Some(PokemonVolatileStatusType::BurningBulwark),
        "charge" => Some(PokemonVolatileStatusType::Charge),
        "curse" => Some(PokemonVolatileStatusType::Curse),
        "defensecurl" => Some(PokemonVolatileStatusType::DefenseCurl),
        "destinybond" => Some(PokemonVolatileStatusType::DestinyBond),
        "protect" => Some(PokemonVolatileStatusType::Protect),
        "disable" => Some(PokemonVolatileStatusType::Disable),
        "dragoncheer" => Some(PokemonVolatileStatusType::DragonCheer),
        "electrify" => Some(PokemonVolatileStatusType::Electrify),
        "embargo" => Some(PokemonVolatileStatusType::Embargo),
        "encore" => Some(PokemonVolatileStatusType::Encore),
        "endure" => Some(PokemonVolatileStatusType::Endure),
        "focusenergy" => Some(PokemonVolatileStatusType::FocusEnergy),
        "followme" => Some(PokemonVolatileStatusType::FollowMe),
        "foresight" => Some(PokemonVolatileStatusType::Foresight),
        "gastroacid" => Some(PokemonVolatileStatusType::GastroAcid),
        "glaiverush" => Some(PokemonVolatileStatusType::GlaiveRush),
        "grudge" => Some(PokemonVolatileStatusType::Grudge),
        "healblock" => Some(PokemonVolatileStatusType::HealBlock),
        "helpinghand" => Some(PokemonVolatileStatusType::HelpingHand),
        "imprison" => Some(PokemonVolatileStatusType::Imprison),
        "ingrain" => Some(PokemonVolatileStatusType::Ingrain),
        "kingsshield" => Some(PokemonVolatileStatusType::KingsShield),
        "laserfocus" => Some(PokemonVolatileStatusType::LaserFocus),
        "leechseed" => Some(PokemonVolatileStatusType::LeechSeed),
        "magiccoat" => Some(PokemonVolatileStatusType::MagicCoat),
        "magnetrise" => Some(PokemonVolatileStatusType::MagnetRise),
        "maxguard" => Some(PokemonVolatileStatusType::MaxGuard),
        "minimize" => Some(PokemonVolatileStatusType::Minimize),
        "miracleeye" => Some(PokemonVolatileStatusType::MiracleEye),
        "nightmare" => Some(PokemonVolatileStatusType::NightMare),
        "noretreat" => Some(PokemonVolatileStatusType::NoRetreat),
        "obstruct" => Some(PokemonVolatileStatusType::Obstruct),
        "octolock" => Some(PokemonVolatileStatusType::OctoLock),
        "lockedmove" => Some(PokemonVolatileStatusType::LockedMove),
        "powder" => Some(PokemonVolatileStatusType::Powder),
        "powershift" => Some(PokemonVolatileStatusType::PowerShift),
        "powertrick" => Some(PokemonVolatileStatusType::PowerTrick),
        "rage" => Some(PokemonVolatileStatusType::Rage),
        "ragepowder" => Some(PokemonVolatileStatusType::RagePowder),
        "roost" => Some(PokemonVolatileStatusType::Roost),
        "saltcure" => Some(PokemonVolatileStatusType::SaltCure),
        "substitute" => Some(PokemonVolatileStatusType::Substitute),
        "silktrap" => Some(PokemonVolatileStatusType::SilkTrap),
        "smackdown" => Some(PokemonVolatileStatusType::SmackDown),
        "snatch" => Some(PokemonVolatileStatusType::Snatch),
        "sparklingaria" => Some(PokemonVolatileStatusType::SparklingAria),
        "spikyshield" => Some(PokemonVolatileStatusType::SpikyShield),
        "spotlight" => Some(PokemonVolatileStatusType::Spotlight),
        "stockpile" => Some(PokemonVolatileStatusType::Stockpile),
        "syrupbomb" => Some(PokemonVolatileStatusType::SyrupBomb),
        "tarshot" => Some(PokemonVolatileStatusType::TarShot),
        "taunt" => Some(PokemonVolatileStatusType::Taunt),
        "telekinesis" => Some(PokemonVolatileStatusType::Telekinesis),
        "torment" => Some(PokemonVolatileStatusType::Torment),
        "uproar" => Some(PokemonVolatileStatusType::Uproar),
        "yawn" => Some(PokemonVolatileStatusType::Yawn),
        _ => None,
    }
}

fn parse_side_condition(s: &str) -> Option<PokemonSideCondition> {
    match s {
        "auroraveil" => Some(PokemonSideCondition::AuroraVeil),
        "reflect" => Some(PokemonSideCondition::Reflect),
        "craftyshield" => Some(PokemonSideCondition::CraftyShield),
        "lightscreen" => Some(PokemonSideCondition::LightScreen),
        "luckychant" => Some(PokemonSideCondition::LuckyChant),
        "matblock" => Some(PokemonSideCondition::MatBlock),
        "mist" => Some(PokemonSideCondition::Mist),
        "quickguard" => Some(PokemonSideCondition::QuickGuard),
        "safeguard" => Some(PokemonSideCondition::SafeGuard),
        "spikes" => Some(PokemonSideCondition::Spikes),
        "stealthrock" => Some(PokemonSideCondition::StealthRock),
        "stickyweb" => Some(PokemonSideCondition::StickyWeb),
        "tailwind" => Some(PokemonSideCondition::TailWind),
        "toxicspikes" => Some(PokemonSideCondition::ToxicSpikes),
        "wideguard" => Some(PokemonSideCondition::WideGuard),
        _ => None,
    }
}

fn parse_terrain(s: &str) -> Option<PokemonTerrain> {
    match s {
        "electricterrain" => Some(PokemonTerrain::ElectricTerrain),
        "grassyterrain" => Some(PokemonTerrain::GrassyTerrain),
        "mistyterrain" => Some(PokemonTerrain::MistyTerrain),
        "psychicterrain" => Some(PokemonTerrain::PsychicTerrain),
        _ => None,
    }
}

fn parse_weather_val(s: &str) -> Option<PokemonWeather> {
    match s {
        "raindance" | "primordialsea" => Some(PokemonWeather::Rain),
        "sunnyday" | "desolateland" => Some(PokemonWeather::Sun),
        "sandstorm" | "sandsear" => Some(PokemonWeather::Sandstorm),
        "hail" | "snowscape" | "snow" => Some(PokemonWeather::Snow),
        _ => None,
    }
}

fn parse_pseudo_weather(s: &str) -> Option<PokemonPseudoWeather> {
    match s {
        "fairylock" => Some(PokemonPseudoWeather::FairyLock),
        "gravity" => Some(PokemonPseudoWeather::Gravity),
        "iondeluge" => Some(PokemonPseudoWeather::IonDeluge),
        "magicroom" => Some(PokemonPseudoWeather::MagicDeluge),
        "mudsport" => Some(PokemonPseudoWeather::MudSport),
        "watersport" => Some(PokemonPseudoWeather::WaterSport),
        "wonderroom" => Some(PokemonPseudoWeather::WonderRoom),
        _ => None,
    }
}

fn parse_slot_condition(s: &str) -> Option<PokemonSlotCondition> {
    match s {
        "futuremove" | "futuresight" | "doomdesire" => Some(PokemonSlotCondition::FutureMove),
        "healingwish" => Some(PokemonSlotCondition::HealingWish),
        "lunardance" => Some(PokemonSlotCondition::LunarDance),
        "revivalblessing" => Some(PokemonSlotCondition::RevivalBlessing),
        "wish" => Some(PokemonSlotCondition::Wish),
        _ => None,
    }
}

fn empty_hit_effect() -> PokemonHitEffect {
    PokemonHitEffect {
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

/// Extracts the `self: { ... }` sub-block from a block of text.
/// Returns (text_without_self_block, Some(self_block_inner_text)).
fn extract_self_subblock(text: &str) -> (String, Option<String>) {
    let mut search_start = 0;
    while let Some(rel_pos) = text[search_start..].find("self:") {
        let abs_pos = search_start + rel_pos;
        // Ensure it's a word boundary (not part of "selfSwitch:", "selfdestruct:", etc.)
        let prev_char = if abs_pos == 0 { ' ' } else { text[..abs_pos].chars().last().unwrap_or(' ') };
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

/// Parse boosts/status/volatileStatus from a text fragment into a PokemonHitEffect.
fn parse_effect_from_text(text: &str) -> PokemonHitEffect {
    let mut effect = empty_hit_effect();
    if let Some(s) = extract_quoted(text, "status") {
        effect.status = parse_nvstatus(&s);
    }
    if let Some(s) = extract_quoted(text, "volatileStatus") {
        effect.volatile_status = parse_volatile(&s);
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
        let val_str: String = rest.chars()
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
                if let (Ok(a), Ok(b)) = (
                    nums[0].trim().parse::<u8>(),
                    nums[1].trim().parse::<u8>(),
                ) {
                    return Some([a, b]);
                }
            }
        }
    }
    None
}

/// Parse boosts from a block of text like `{ atk: -1, def: 2 }`
/// Returns [atk, def, spa, spd, spe, accuracy, evasion, 0]
fn parse_boosts_from_text(text: &str) -> PokemonBoostArr {
    let mut boosts = [0i8; 7];
    for part in text.split(',') {
        let part = part.trim();
        if let Some((key, val)) = part.split_once(':') {
            let key = key.trim().trim_matches(|c: char| !c.is_alphanumeric());
            let val: i8 = val.trim().trim_matches(|c: char| !c.is_ascii_digit() && c != '-')
                .parse().unwrap_or(0);
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
fn parse_flags_from_text(text: &str) -> Vec<PokemonMoveFlag> {
    let mut flags = Vec::new();
    let inner = text.trim().trim_start_matches('{').trim_end_matches('}');
    for part in inner.split(',') {
        if let Some((key, _)) = part.split_once(':') {
            if let Some(flag) = parse_flag(key.trim()) {
                flags.push(flag);
            }
        }
    }
    flags
}

fn parse_damage_override(s: &str) -> Option<PokemonDamageOverride> {
    match s {
        "level" => Some(PokemonDamageOverride::Level),
        _ => Some(PokemonDamageOverride::Number(s.parse().ok()?)),
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
            let key = trimmed.split(':').next().unwrap_or("").trim()
                .trim_matches('"').trim_matches('\'').to_string();
            current_key = key;
            current_lines.clear();
            in_entry = true;
            depth += open - close;
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
fn parse_secondary_block(lines: &[String], start_idx: usize)
    -> (Option<PokemonSecondaryEffect>, Option<PokemonSecondaryEffect>, usize)
{
    let (block_text, end_idx) = collect_block(lines, start_idx);

    // Parse chance
    let chance: u8 = extract_int(&block_text, "chance").unwrap_or(0);

    // Separate self: { ... } from the rest
    let (target_text, self_text) = extract_self_subblock(&block_text);

    // Parse target effects
    let target_effect = parse_effect_from_text(&target_text);
    let has_target = chance > 0
        || target_effect.status.is_some()
        || target_effect.volatile_status.is_some()
        || target_effect.boosts.iter().any(|&b| b != 0);
    let target_sec = if has_target {
        Some(PokemonSecondaryEffect { chance, effect: target_effect })
    } else {
        None
    };

    // Parse self effects
    let self_sec = if let Some(st) = self_text {
        let self_effect = parse_effect_from_text(&st);
        let has_self = self_effect.status.is_some()
            || self_effect.volatile_status.is_some()
            || self_effect.boosts.iter().any(|&b| b != 0);
        if has_self {
            Some(PokemonSecondaryEffect { chance, effect: self_effect })
        } else {
            None
        }
    } else {
        None
    };

    (target_sec, self_sec, end_idx)
}

// --- Public Dex Parsing ---

/// Parse showdownDex.txt into a HashMap of PokemonData keyed by species id.
pub fn parse_pokemon_dex(file_path: &str) -> HashMap<String, PokemonData> {
    let content = fs::read_to_string(file_path).expect("Failed to read Pokemon dex file");
    let entries = split_entries(&content);
    let mut result = HashMap::new();

    for (key, lines) in &entries {
        let mut species = String::new();
        let mut types: Vec<PokemonType> = Vec::new();
        let mut base_stats = [0u16; 6];
        let mut weight: u16 = 0;

        for line in lines {
            let trimmed = line.trim();

            if trimmed.starts_with("name:") {
                if let Some(name) = extract_quoted(trimmed, "name") {
                    species = name;
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
                    let val_str: String = rest.trim().trim_end_matches(',')
                        .chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
                    if let Ok(w) = val_str.parse::<f64>() {
                        weight = (w * 10.0).round() as u16; // hectograms
                    }
                }
            }
        }

        if !species.is_empty() {
            result.insert(key.clone(), PokemonData {
                species,
                types,
                base_stats,
                weight,
            });
        }
    }

    result
}

/// Parse showdownMoves.txt into a HashMap of PokemonMoveData keyed by move id.
pub fn parse_move_dex(file_path: &str) -> HashMap<String, PokemonMoveData> {
    let content = fs::read_to_string(file_path).expect("Failed to read moves file");
    let entries = split_entries(&content);
    let mut result = HashMap::new();

    for (key, lines) in &entries {
        let mut name = String::new();
        let mut accuracy = PokemonAccuracy::Percent(100);
        let mut pp: u8 = 0;
        let mut category = PokemonMoveCategory::Status;
        let mut pokemon_type = PokemonType::Normal;
        let mut priority: i8 = 0;
        let mut target = PokemonMoveTarget::Normal;
        let mut flags: Vec<PokemonMoveFlag> = Vec::new();
        let mut base_power: u16 = 0;

        let mut ohko = false;
        let mut thaws_target = false;
        let mut heal_fraction: [u8; 2] = [0, 0];
        let mut force_switch = false;
        let mut self_switch = PokemonSelfSwitchType::None;
        let mut self_boost = [0i8; 7];
        let mut top_level_boosts: Option<[i8; 7]> = None;
        let mut self_destruct = PokemonSelfDestructType::None;
        let mut breaks_protect = false;
        let mut recoil_fraction: [u8; 2] = [0, 0];
        let mut drain_fraction: [u8; 2] = [0, 0];
        let mut mind_blown_recoil = false;
        let mut struggle_recoil = false;
        let mut steals_boosts = false;

        let mut secondaries: Vec<PokemonSecondaryEffect> = Vec::new();
        let mut self_secondaries: Vec<PokemonSecondaryEffect> = Vec::new();

        let mut crit_ratio: u8 = 0;
        let mut foul_play = false;

        let mut ignore_ability = false;
        let mut ignore_defense_boosts = false;
        let mut ignore_evasion = false;
        let mut ignore_immunity: Vec<PokemonType> = Vec::new();

        let mut multihit_range: [u8; 2] = [0, 0];
        let mut multihit_accuracy = false;

        let mut sleep_usable = false;
        let mut smart_target = false;
        let mut tracks_target = false;
        let mut calls_move = false;
        let mut has_crash_damage = false;
        let mut stalling_move = false;
        let mut override_offensive_stat: Option<PokemonStat> = None;
        let mut override_defensive_stat: Option<PokemonStat> = None;

        let mut i = 0;
        let mut depth: i32 = 0;
        let mut skip_until_depth: Option<i32> = None;
        let mut damage_override: PokemonDamageOverride = PokemonDamageOverride::None;

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
                    name = n;
                }
            } else if trimmed.starts_with("accuracy:") {
                if extract_bool(trimmed, "accuracy") == Some(true) {
                    accuracy = PokemonAccuracy::True;
                } else if let Some(v) = extract_int::<u8>(trimmed, "accuracy") {
                    accuracy = PokemonAccuracy::Percent(v);
                }
            } else if trimmed.starts_with("pp:") {
                if let Some(v) = extract_int::<u8>(trimmed, "pp") {
                    pp = v;
                }
            } else if trimmed.starts_with("basePower:") {
                if let Some(v) = extract_int::<u16>(trimmed, "basePower") {
                    base_power = v;
                }
            }else if trimmed.starts_with("category:") {
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
                    self_switch = PokemonSelfSwitchType::Normal;
                } else if let Some(s) = extract_quoted(trimmed, "selfSwitch") {
                    self_switch = match s.as_str() {
                        "shedtail" => PokemonSelfSwitchType::ShedTail,
                        "copyvolatile" => PokemonSelfSwitchType::BatonPass,
                        _ => PokemonSelfSwitchType::Normal,
                    };
                }
            } else if trimmed.starts_with("selfdestruct:") {
                if let Some(s) = extract_quoted(trimmed, "selfdestruct") {
                    self_destruct = match s.as_str() {
                        "always" => PokemonSelfDestructType::Always,
                        "ifHit" => PokemonSelfDestructType::IfHit,
                        _ => PokemonSelfDestructType::None,
                    };
                }
            } else if trimmed.starts_with("breaksProtect:") {
                breaks_protect = extract_bool(trimmed, "breaksProtect").unwrap_or(false);
            } else if trimmed.starts_with("mindBlownRecoil:") {
                mind_blown_recoil = extract_bool(trimmed, "mindBlownRecoil").unwrap_or(false);
            } else if trimmed.starts_with("struggleRecoil:") {
                struggle_recoil = extract_bool(trimmed, "struggleRecoil").unwrap_or(false);
            } else if trimmed.starts_with("stealsBoosts:") {
                steals_boosts = extract_bool(trimmed, "stealsBoosts").unwrap_or(false);
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
                        PokemonType::Normal, PokemonType::Fire, PokemonType::Water,
                        PokemonType::Electric, PokemonType::Grass, PokemonType::Ice,
                        PokemonType::Fighting, PokemonType::Poison, PokemonType::Ground,
                        PokemonType::Flying, PokemonType::Psychic, PokemonType::Bug,
                        PokemonType::Rock, PokemonType::Ghost, PokemonType::Dragon,
                        PokemonType::Dark, PokemonType::Steel, PokemonType::Fairy,
                    ];
                } else if trimmed.contains('{') && trimmed.contains('}') {
                    // ignoreImmunity: { 'Fairy': true }
                    let inner = &trimmed[trimmed.find('{').unwrap()..=trimmed.rfind('}').unwrap()];
                    for part in inner.split(',') {
                        let part = part.trim().trim_matches(|c: char| c == '{' || c == '}' || c == '\'' || c == '"');
                        if let Some((type_name, _)) = part.split_once(':') {
                            let type_name = type_name.trim().trim_matches(|c: char| c == '\'' || c == '"');
                            if let Some(pt) = parse_type(type_name) {
                                ignore_immunity.push(pt);
                            }
                        }
                    }
                }
            } else if trimmed.starts_with("sleepUsable:") {
                sleep_usable = extract_bool(trimmed, "sleepUsable").unwrap_or(false);
            } else if trimmed.starts_with("smartTarget:") {
                smart_target = extract_bool(trimmed, "smartTarget").unwrap_or(false);
            } else if trimmed.starts_with("tracksTarget:") {
                tracks_target = extract_bool(trimmed, "tracksTarget").unwrap_or(false);
            } else if trimmed.starts_with("callsMove:") {
                calls_move = extract_bool(trimmed, "callsMove").unwrap_or(false);
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
                    if let Some(s) = target_sec { secondaries.push(s); }
                    if let Some(s) = self_sec { self_secondaries.push(s); }
                    i = end;
                }
            } else if trimmed.starts_with("secondaries:") {
                if trimmed.contains('[') {
                    let (_block, end) = collect_block(lines, i);
                    let block_lines: Vec<String> = lines[i..=end]
                        .iter().map(|s| s.to_string()).collect();
                    let mut k = 0;
                    while k < block_lines.len() {
                        let bl = block_lines[k].trim();
                        if bl.starts_with('{') || bl.contains("chance:") {
                            let (target_sec, self_sec, sec_end) = parse_secondary_block(&block_lines, k);
                            if let Some(s) = target_sec { secondaries.push(s); }
                            if let Some(s) = self_sec { self_secondaries.push(s); }
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
                        secondaries.push(PokemonSecondaryEffect { chance: 100, effect: e });
                    }
                }
            } else if trimmed.starts_with("volatileStatus:") && depth <= 1 {
                // Top-level volatileStatus: always-apply 100% secondary
                if let Some(s) = extract_quoted(trimmed, "volatileStatus") {
                    if let Some(vs) = parse_volatile(&s) {
                        let mut e = empty_hit_effect();
                        e.volatile_status = Some(vs);
                        secondaries.push(PokemonSecondaryEffect { chance: 100, effect: e });
                    }
                }
            } else if trimmed.starts_with("sideCondition:") && depth <= 1 {
                if let Some(s) = extract_quoted(trimmed, "sideCondition") {
                    if let Some(sc) = parse_side_condition(&s) {
                        let mut e = empty_hit_effect();
                        e.side_condition = Some(sc);
                        secondaries.push(PokemonSecondaryEffect { chance: 100, effect: e });
                    }
                }
            } else if trimmed.starts_with("terrain:") && depth <= 1 {
                if let Some(s) = extract_quoted(trimmed, "terrain") {
                    if let Some(t) = parse_terrain(&s) {
                        let mut e = empty_hit_effect();
                        e.terrain = Some(t);
                        secondaries.push(PokemonSecondaryEffect { chance: 100, effect: e });
                    }
                }
            } else if trimmed.starts_with("weather:") && depth <= 1 {
                if let Some(s) = extract_quoted(trimmed, "weather") {
                    if let Some(w) = parse_weather_val(&s) {
                        let mut e = empty_hit_effect();
                        e.weather = Some(w);
                        secondaries.push(PokemonSecondaryEffect { chance: 100, effect: e });
                    }
                }
            } else if trimmed.starts_with("pseudoWeather:") && depth <= 1 {
                if let Some(s) = extract_quoted(trimmed, "pseudoWeather") {
                    if let Some(pw) = parse_pseudo_weather(&s) {
                        let mut e = empty_hit_effect();
                        e.pseudo_weather = Some(pw);
                        secondaries.push(PokemonSecondaryEffect { chance: 100, effect: e });
                    }
                }
            } else if trimmed.starts_with("slotCondition:") && depth <= 1 {
                if let Some(s) = extract_quoted(trimmed, "slotCondition") {
                    if let Some(slc) = parse_slot_condition(&s) {
                        let mut e = empty_hit_effect();
                        e.slot_condition = Some(slc);
                        secondaries.push(PokemonSecondaryEffect { chance: 100, effect: e });
                    }
                }
            } else if trimmed.starts_with("self:") && !trimmed.starts_with("selfSwitch") && !trimmed.starts_with("selfdestruct") && depth <= 1 {
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
                                self_secondaries.push(PokemonSecondaryEffect {
                                    chance: 100,
                                    effect: self_effect,
                                });
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
                    damage_override = parse_damage_override(&s).unwrap_or(PokemonDamageOverride::None);
                } else if let Some(v) = extract_int::<u16>(trimmed, "damage") {
                    damage_override = PokemonDamageOverride::Number(v);
                }
            } else if trimmed.starts_with("damageOverride:") {
                if let Some(s) = extract_quoted(trimmed, "damageOverride") {
                    damage_override = parse_damage_override(&s).unwrap_or(PokemonDamageOverride::None);
                }
            }

            i += 1;
        }

        // Decide where top-level boosts belong based on move target
        if let Some(boosts) = top_level_boosts {
            let self_targeting = matches!(
                target,
                PokemonMoveTarget::SelfTarget
                    | PokemonMoveTarget::AllySide
                    | PokemonMoveTarget::AllyTeam
            );
            if self_targeting {
                self_boost = boosts;
            } else {
                let mut e = empty_hit_effect();
                e.boosts = boosts;
                secondaries.push(PokemonSecondaryEffect { chance: 100, effect: e });
            }
        }

        if !name.is_empty() {
            result.insert(key.clone(), PokemonMoveData {
                name,
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
                steals_boosts,
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
                smart_target,
                tracks_target,
                calls_move,
                has_crash_damage,
                damage_override,
                stalling_move,
                override_offensive_stat,
                override_defensive_stat,
            });
        }
    }

    result
}
