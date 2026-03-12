use crate::dex_data::{PokemonBoostTable, PokemonData, PokemonType, VolatileStatus, Status, parse_type};
use std::collections::HashMap;
use std::fs;

pub type PokemonStatsTable = [u16; 6]; // hp, atk, def, spa, spd, spe

#[derive(Debug, Clone)]
pub enum VolatileStatusState{
    Status(VolatileStatus, u8),//Make this hold more information about each status, like turns remaining, etc.
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PokemonGender{
    Male,
    Female,
    Genderless
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Nature{
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
    Serious
}

#[derive(Debug, Clone)]
pub struct PokemonState{
    pub fainted: bool,
    pub species: String,//should be all lower case, all non-alpha characters removed
    pub types: Vec<PokemonType>,
    pub tera_type: PokemonType,
    pub is_tera: bool,

    pub stats: PokemonStatsTable,
    pub boosts: PokemonBoostTable,

    pub level: u8,
    pub hp: u16,
    pub moves: [String; 4],
    pub item: String,
    pub nature: Nature,

    pub status: Option<Status>,
    pub volatiles: Vec<VolatileStatusState>,

    pub base_ability: String,
    pub ability: String,

    pub last_move_failed: bool,//For stomping tantrum
    
    pub gender: PokemonGender,
    pub weight_hg: u16,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Player{
    P1,
    P2,
}

#[derive(Debug)]
pub struct FieldSlot{
    pub player: Player,
    pub slot_index: u8,
}

#[derive(Debug)]
pub struct MoveAction{
    pub move_name: String,
    pub priority: i8,
    pub user_slot: FieldSlot,
    pub target_slot: FieldSlot,
}

#[derive(Debug)]
pub struct SwitchAction{
    pub speed: u16,
    pub user_slot: FieldSlot,
    pub switch_index: usize,
}

#[derive(Debug)]
pub enum Action{
    MoveAction(MoveAction),
    SwitchAction(SwitchAction),
}

#[derive(Debug)]
pub struct BattleState{
    pub active_per_side: u8,
    
    pub p1_active_mons: Vec<PokemonState>,
    pub p2_active_mons: Vec<PokemonState>,
    pub p1_back_mons: Vec<PokemonState>,
    pub p2_back_mons: Vec<PokemonState>,
    
    pub p1_has_tera: bool,
    pub p2_has_tera: bool,

    pub action_queue: Vec<Action>,

    pub turn_number: u16,

    pub turn_started: bool,
    pub turn_ended: bool,
}

#[derive(Debug)]
pub struct TeamPreviewState{
    pub active_per_side: u8,
    pub p1_mons: Vec<PokemonState>,
    pub p2_mons: Vec<PokemonState>,
}

#[derive(Debug)]
pub enum MatchState{
    BattleState(BattleState),
    TeamPreviewState(TeamPreviewState),
    GameOverState{winner:Player},
}

// --- Stat Calculation Helpers ---

fn normalize_string(name: impl AsRef<str>) -> String {
    name.as_ref()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn parse_nature_str(s: &str) -> Option<Nature> {
    match s.trim() {
        "Hardy"   => Some(Nature::Hardy),
        "Lonely"  => Some(Nature::Lonely),
        "Adamant" => Some(Nature::Adamant),
        "Naughty" => Some(Nature::Naughty),
        "Brave"   => Some(Nature::Brave),
        "Bold"    => Some(Nature::Bold),
        "Docile"  => Some(Nature::Docile),
        "Impish"  => Some(Nature::Impish),
        "Lax"     => Some(Nature::Lax),
        "Relaxed" => Some(Nature::Relaxed),
        "Modest"  => Some(Nature::Modest),
        "Mild"    => Some(Nature::Mild),
        "Bashful" => Some(Nature::Bashful),
        "Rash"    => Some(Nature::Rash),
        "Quiet"   => Some(Nature::Quiet),
        "Calm"    => Some(Nature::Calm),
        "Gentle"  => Some(Nature::Gentle),
        "Careful" => Some(Nature::Careful),
        "Quirky"  => Some(Nature::Quirky),
        "Sassy"   => Some(Nature::Sassy),
        "Timid"   => Some(Nature::Timid),
        "Hasty"   => Some(Nature::Hasty),
        "Jolly"   => Some(Nature::Jolly),
        "Naive"   => Some(Nature::Naive),
        "Serious" => Some(Nature::Serious),
        _         => None,
    }
}



/// Returns stat multipliers as [atk, def, spa, spd, spe] for the given nature.
fn nature_stat_modifiers(nature: &Nature) -> [f32; 5] {
    match nature {
        Nature::Hardy | Nature::Docile | Nature::Bashful | Nature::Quirky | Nature::Serious
            => [1.0, 1.0, 1.0, 1.0, 1.0],
        // +Atk
        Nature::Lonely  => [1.1, 0.9, 1.0, 1.0, 1.0],
        Nature::Adamant => [1.1, 1.0, 0.9, 1.0, 1.0],
        Nature::Naughty => [1.1, 1.0, 1.0, 0.9, 1.0],
        Nature::Brave   => [1.1, 1.0, 1.0, 1.0, 0.9],
        // +Def
        Nature::Bold    => [0.9, 1.1, 1.0, 1.0, 1.0],
        Nature::Impish  => [1.0, 1.1, 0.9, 1.0, 1.0],
        Nature::Lax     => [1.0, 1.1, 1.0, 0.9, 1.0],
        Nature::Relaxed => [1.0, 1.1, 1.0, 1.0, 0.9],
        // +SpA
        Nature::Modest  => [0.9, 1.0, 1.1, 1.0, 1.0],
        Nature::Mild    => [1.0, 0.9, 1.1, 1.0, 1.0],
        Nature::Rash    => [1.0, 1.0, 1.1, 0.9, 1.0],
        Nature::Quiet   => [1.0, 1.0, 1.1, 1.0, 0.9],
        // +SpD
        Nature::Calm    => [0.9, 1.0, 1.0, 1.1, 1.0],
        Nature::Gentle  => [1.0, 0.9, 1.0, 1.1, 1.0],
        Nature::Careful => [1.0, 1.0, 0.9, 1.1, 1.0],
        Nature::Sassy   => [1.0, 1.0, 1.0, 1.1, 0.9],
        // +Spe
        Nature::Timid   => [0.9, 1.0, 1.0, 1.0, 1.1],
        Nature::Hasty   => [1.0, 0.9, 1.0, 1.0, 1.1],
        Nature::Jolly   => [1.0, 1.0, 0.9, 1.0, 1.1],
        Nature::Naive   => [1.0, 1.0, 1.0, 0.9, 1.1],
    }
}

fn calc_hp(base: u16, iv: u8, ev: u8, level: u8) -> u16 {
    let ev_contrib = ev as u16 / 4;
    (2 * base + iv as u16 + ev_contrib) * level as u16 / 100 + level as u16 + 10
}

fn calc_stat(base: u16, iv: u8, ev: u8, level: u8, nature_mod: f32) -> u16 {
    let ev_contrib = ev as u16 / 4;
    let inner = (2 * base + iv as u16 + ev_contrib) * level as u16 / 100 + 5;
    (inner as f32 * nature_mod).floor() as u16
}

// --- Team Sheet Parsing ---

/// Parses a Showdown-format teamsheet file and returns a Vec of PokemonStates.
/// Each Pokemon's stats are calculated from base stats, EVs, IVs, level, and nature.
pub fn parse_team_sheet(path: &str, pokemon_dex: &HashMap<String, PokemonData>) -> Vec<PokemonState> {
    let content = fs::read_to_string(path).expect("Failed to read team sheet file");
    // Normalize line endings so blank-line splitting works on Windows files too
    let content = content.replace("\r\n", "\n");
    // Blocks are separated by blank lines
    let blocks: Vec<&str> = content.split("\n\n").collect();
    let mut team = Vec::new();

    for block in blocks {
        let lines: Vec<&str> = block.lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();

        if lines.is_empty() {
            continue;
        }

        // --- Parse header: "Nickname (Species) (Gender) @ Item" ---
        // Showdown formats:
        //   Species @ Item
        //   Species (M) @ Item
        //   Nickname (Species) @ Item
        //   Nickname (Species) (M) @ Item
        let header = lines[0];
        let (name_part, item) = if let Some(at_idx) = header.find(" @ ") {
            (&header[..at_idx], header[at_idx + 3..].trim().to_string())
        } else {
            (header, String::new())
        };

        // Collect all parenthesized groups in order
        let mut paren_groups: Vec<&str> = Vec::new();
        let mut search = name_part;
        while let (Some(op), Some(cp)) = (search.find('('), search.find(')')) {
            paren_groups.push(&search[op + 1..cp]);
            search = &search[cp + 1..];
        }

        // The text before the first '(' is either the species (no nickname) or the nickname
        let text_before_parens = if let Some(op) = name_part.find('(') {
            name_part[..op].trim()
        } else {
            name_part.trim()
        };

        // Check if the last paren group is a gender marker
        let gender = match paren_groups.last() {
            Some(&"M") => PokemonGender::Male,
            Some(&"F") => PokemonGender::Female,
            _          => PokemonGender::Genderless,
        };
        let non_gender_groups: Vec<&str> = paren_groups.iter()
            .filter(|&&g| g != "M" && g != "F")
            .copied()
            .collect();

        // The species name: last non-gender paren group if present, else text_before_parens
        let species_name = non_gender_groups.last().copied().unwrap_or(text_before_parens);
        let base_name = text_before_parens; // used only for error messages

        // Determine dex lookup key
        let species_key = {
            let key = normalize_string(species_name);
            if pokemon_dex.contains_key(&key) {
                key
            } else {
                normalize_string(text_before_parens)
            }
        };

        // --- Parse remaining lines ---
        let mut ability = String::new();
        let mut level: u8 = 50;
        let mut tera_type = PokemonType::Normal;
        let mut evs = [0u8; 6]; // hp, atk, def, spa, spd, spe
        let mut ivs = [31u8; 6];
        let mut nature = Nature::Hardy;
        let mut moves: [String; 4] = Default::default();
        let mut move_count = 0;

        for &line in &lines[1..] {
            if let Some(rest) = line.strip_prefix("Ability:") {
                ability = rest.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("Level:") {
                level = rest.trim().parse().unwrap_or(50);
            } else if let Some(rest) = line.strip_prefix("Tera Type:") {
                tera_type = parse_type(rest.trim()).unwrap_or(PokemonType::Normal);
            } else if let Some(rest) = line.strip_prefix("EVs:") {
                for part in rest.split('/') {
                    let part = part.trim();
                    let mut iter = part.splitn(2, ' ');
                    if let (Some(num_str), Some(stat_name)) = (iter.next(), iter.next()) {
                        if let Ok(val) = num_str.trim().parse::<u8>() {
                            match stat_name.trim() {
                                "HP"  => evs[0] = val,
                                "Atk" => evs[1] = val,
                                "Def" => evs[2] = val,
                                "SpA" => evs[3] = val,
                                "SpD" => evs[4] = val,
                                "Spe" => evs[5] = val,
                                _ => {}
                            }
                        }
                    }
                }
            } else if let Some(rest) = line.strip_prefix("IVs:") {
                for part in rest.split('/') {
                    let part = part.trim();
                    let mut iter = part.splitn(2, ' ');
                    if let (Some(num_str), Some(stat_name)) = (iter.next(), iter.next()) {
                        if let Ok(val) = num_str.trim().parse::<u8>() {
                            match stat_name.trim() {
                                "HP"  => ivs[0] = val,
                                "Atk" => ivs[1] = val,
                                "Def" => ivs[2] = val,
                                "SpA" => ivs[3] = val,
                                "SpD" => ivs[4] = val,
                                "Spe" => ivs[5] = val,
                                _ => {}
                            }
                        }
                    }
                }
            } else if let Some(nature_str) = line.strip_suffix(" Nature") {
                nature = parse_nature_str(nature_str).unwrap_or(Nature::Hardy);
            } else if let Some(move_str) = line.strip_prefix("- ") {
                if move_count < 4 {
                    moves[move_count] = normalize_string(move_str);
                    move_count += 1;
                }
            }
        }

        // --- Look up dex data ---
        let (types, base_stats, weight_hg) = match pokemon_dex.get(&species_key) {
            Some(data) => (data.types.clone(), data.base_stats, data.weight),
            None => {
                eprintln!("Warning: '{}' not found in dex (key: '{}')", base_name, species_key);
                (vec![PokemonType::Normal], [100u16; 6], 0u16)
            }
        };

        // --- Compute actual stats from base stats, EVs, IVs, level, nature ---
        let hp = calc_hp(base_stats[0], ivs[0], evs[0], level);
        let mods = nature_stat_modifiers(&nature);
        // stats layout: [hp, atk, def, spa, spd, spe]
        let stats: PokemonStatsTable = [
            hp,
            calc_stat(base_stats[1], ivs[1], evs[1], level, mods[0]),
            calc_stat(base_stats[2], ivs[2], evs[2], level, mods[1]),
            calc_stat(base_stats[3], ivs[3], evs[3], level, mods[2]),
            calc_stat(base_stats[4], ivs[4], evs[4], level, mods[3]),
            calc_stat(base_stats[5], ivs[5], evs[5], level, mods[4]),
        ];

        let normalized_ability = normalize_string(&ability);
        team.push(PokemonState {
            fainted: false,
            species: species_key,
            types,
            tera_type,
            is_tera: false,
            stats,
            boosts: [0; 7],
            level,
            hp,
            moves,
            item: normalize_string(item),
            nature,
            status: None,
            volatiles: Vec::new(),
            base_ability: normalized_ability.clone(),
            ability: normalized_ability,
            last_move_failed: false,
            gender,
            weight_hg,
        });
    }

    team
}

/// Builds a TeamPreviewState from two teamsheet file paths.
pub fn team_preview_state_from_teamsheets(
    p1_path: &str,
    p2_path: &str,
    pokemon_dex: &HashMap<String, PokemonData>,
    active_per_side: u8,
) -> TeamPreviewState {
    TeamPreviewState {
        active_per_side,
        p1_mons: parse_team_sheet(p1_path, pokemon_dex),
        p2_mons: parse_team_sheet(p2_path, pokemon_dex),
    }
}