use crate::dex_data::{PokemonBoostTable, PokemonData, PokemonType, VolatileStatus, Status, parse_type, MoveData};
use crate::data::item::Item;
use std::collections::HashMap;
use std::fs;
use crate::data::species::Species;
use crate::data::ability::Ability;
use crate::data::pokemon_move::PokemonMove;

pub type PokemonStatsTable = [u16; 6]; // hp, atk, def, spa, spd, spe

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

fn move_name(mov: &PokemonMove) -> String {
    humanize_identifier(format!("{:?}", mov))
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VolatileStatusState{
    TurnStatus(VolatileStatus, u8),
    MoveStatus(VolatileStatus, u8),
    Charging(PokemonMove, Vec<crate::battle::FieldSlot>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PokemonGender{
    Male,
    Female,
    Genderless
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PokemonState{
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
    pub item: Item,
    pub nature: Nature,

    pub boosts: PokemonBoostTable,
    pub stats: PokemonStatsTable,
    
    pub status: Option<Status>,
    pub volatiles: Vec<VolatileStatusState>,

    pub base_ability: Ability,
    pub ability: Ability,

    pub gender: PokemonGender,
    pub weight_hg: u16,

    pub tera_type: PokemonType,

    pub mega_species: Option<Species>,
    pub mega_ability: Option<Ability>,

    pub last_move_failed: bool,//For stomping tantrum

    pub evs: [u8; 6],
    pub ivs: [u8; 6],
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

        let evs_str = self.evs.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("/");
        let ivs_str = self.ivs.iter().map(|i| i.to_string()).collect::<Vec<_>>().join("/");

        let vol_str = if self.volatiles.is_empty() {
            "none".to_string()
        } else {
            self.volatiles.iter().map(|v| format!("{:?}", v)).collect::<Vec<_>>().join(", ")
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
                let name = m.as_ref().map(move_name).unwrap_or_else(|| format!("Move {}", i + 1));
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
            .map(|data| data.default_gender)
            .unwrap_or(PokemonGender::Genderless)
    });
    let ability = ability.unwrap_or_else(|| {
        Ability::Illuminate
    });
    let nature = nature.unwrap_or(Nature::Hardy);
    let item = item.unwrap_or(Item::None);
    let tera_type = tera_type.unwrap_or(PokemonType::Normal);
    let mut evs = evs.unwrap_or([0; 6]);
    let ivs = ivs.unwrap_or([31; 6]);

    if use_stat_points {
        for ev in &mut evs {
            *ev = ((i16::from(*ev) * 8) - 4).max(0) as u8;
        }
    }

    let mut move_pp = [0u8; 4];
    for (move_idx, mov) in moves.iter().enumerate() {
        if let Some(mov) = mov {
            if let Some(move_data) = move_dex.get(mov) {
                move_pp[move_idx] = if *mov == PokemonMove::Protect {
                    8
                } else {
                    match move_data.pp {
                        5 => 8,
                        10 => 12,
                        15 => 16,
                        pp if pp > 15 => 20,
                        pp => pp,
                    }
                };
            }
        }
    }

    let (types, base_stats, weight_hg) = match pokemon_dex.get(&species) {
        Some(data) => (data.types.clone(), data.base_stats, data.weight),
        None => (vec![PokemonType::Normal], [100u16; 6], 0u16),
    };

    let stats = calc_stats_for_level(base_stats, ivs, evs, level, &nature);
    let hp = stats[0];

    let normalized_item_str = if item == Item::None {
        String::new()
    } else {
        normalize_string(format!("{:?}", item))
    };
    let is_mega = dex_entry
        .map(|data| is_mega_dex_entry(&species, data))
        .unwrap_or(false);
    let mega_species = if is_mega {
        None
    } else {
        resolve_mega_species(&species, &normalized_item_str, pokemon_dex)
    };
    let mega_ability = mega_species
        .as_ref()
        .and_then(|key| pokemon_dex.get(key))
        .and_then(|data| data.primary_ability.clone());

    PokemonState {
        fainted: false,
        species,
        types,
        is_tera: false,
        is_mega,
        has_mega_form: mega_species.is_some(),
        level,
        hp,
        moves,
        move_pp,
        item,
        nature,
        boosts: [0; 7],
        stats,
        status: None,
        volatiles: Vec::new(),
        base_ability: ability.clone(),
        ability,
        gender,
        weight_hg,
        tera_type,
        mega_species,
        mega_ability,
        last_move_failed: false,
        evs,
        ivs,
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

fn is_mega_dex_entry(species_key: &Species, data: &PokemonData) -> bool {
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

        let matches_base_species = data.base_species.as_ref() == Some(&base_species_key)
            || data.battle_only.as_ref() == Some(&base_species_key);
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

// --- Team Sheet Parsing ---

/// Parses a Showdown-format teamsheet file and returns a Vec of PokemonStates.
/// Each Pokemon's stats are calculated from base stats, EVs, IVs, level, and nature.
pub fn parse_team_sheet(
    path: &str,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    use_stat_points: bool,
) -> Vec<PokemonState> {
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
        let explicit_gender = match paren_groups.last() {
            Some(&"M") => Some(PokemonGender::Male),
            Some(&"F") => Some(PokemonGender::Female),
            _          => None,
        };
        let non_gender_groups: Vec<&str> = paren_groups.iter()
            .filter(|&&g| g != "M" && g != "F")
            .copied()
            .collect();

        // The species name: last non-gender paren group if present, else text_before_parens
        let species_name = non_gender_groups.last().copied().unwrap_or(text_before_parens);
        let base_name = text_before_parens; // used only for error messages

        // Determine dex lookup key
        let species_key_str = {
            let key = normalize_string(species_name);
            if pokemon_dex.contains_key(&Species::from_str(&key)) {
                key
            } else {
                normalize_string(text_before_parens)
            }
        };

        let species_key = Species::from_str(&species_key_str);
        let dex_entry = pokemon_dex.get(&species_key);
        if dex_entry.is_none() {
            eprintln!("Warning: '{}' not found in dex (key: '{:?}')", base_name, species_key);
        }

        // --- Parse remaining lines ---
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
                let mut parsed_evs = evs.unwrap_or([0u8; 6]);
                for part in rest.split('/') {
                    let part = part.trim();
                    let mut iter = part.splitn(2, ' ');
                    if let (Some(num_str), Some(stat_name)) = (iter.next(), iter.next()) {
                        if let Ok(val) = num_str.trim().parse::<u8>() {
                            // Convert from stat points to EVs if needed
                            let ev_value = if use_stat_points {
                                // Apply formula: EV(n) = ((n-4)/8)+1 to convert stat points to EVs
                                ((val as i16 * 8) - 4).max(0) as u8
                            } else {
                                // Normal EV format, use as-is
                                val
                            };
                            
                            match stat_name.trim() {
                                "HP"  => parsed_evs[0] = ev_value,
                                "Atk" => parsed_evs[1] = ev_value,
                                "Def" => parsed_evs[2] = ev_value,
                                "SpA" => parsed_evs[3] = ev_value,
                                "SpD" => parsed_evs[4] = ev_value,
                                "Spe" => parsed_evs[5] = ev_value,
                                _ => {}
                            }
                        }
                    }
                }
                evs = Some(parsed_evs);
            } else if let Some(rest) = line.strip_prefix("IVs:") {
                let mut parsed_ivs = ivs.unwrap_or([31u8; 6]);
                for part in rest.split('/') {
                    let part = part.trim();
                    let mut iter = part.splitn(2, ' ');
                    if let (Some(num_str), Some(stat_name)) = (iter.next(), iter.next()) {
                        if let Ok(val) = num_str.trim().parse::<u8>() {
                            match stat_name.trim() {
                                "HP"  => parsed_ivs[0] = val,
                                "Atk" => parsed_ivs[1] = val,
                                "Def" => parsed_ivs[2] = val,
                                "SpA" => parsed_ivs[3] = val,
                                "SpD" => parsed_ivs[4] = val,
                                "Spe" => parsed_ivs[5] = val,
                                _ => {}
                            }
                        }
                    }
                }
                ivs = Some(parsed_ivs);
            } else if let Some(nature_str) = line.strip_suffix(" Nature") {
                nature = parse_nature_str(nature_str);
            } else if let Some(move_str) = line.strip_prefix("- ") {
                if move_count < 4 {
                    moves[move_count] = Some(PokemonMove::from_str(move_str));
                    move_count += 1;
                }
            }
        }

        let item = if item.is_empty() {
            None
        } else {
            Some(Item::from_str(&item))
        };

        team.push(build_pokemon_state(
            species_key,
            pokemon_dex,
            move_dex,
            level,
            Some(moves),
            explicit_gender,
            ability,
            nature,
            item,
            tera_type,
            evs,
            ivs,
            use_stat_points,
        ));
    }

    team
}

