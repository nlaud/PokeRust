use crate::dex_data::{PokemonBoostTable, PokemonData, PokemonType, VolatileStatus, Status, parse_type, MoveData};
use crate::data::item::Item;
use std::collections::HashMap;
use std::fs;
use crate::data::species::Species;
use crate::data::ability::Ability;
use crate::data::pokemon_move::PokemonMove;

pub type PokemonStatsTable = [u16; 6]; // hp, atk, def, spa, spd, spe

#[derive(Debug, Clone)]
pub enum VolatileStatusState{
    Status(VolatileStatus, u8),//Make this hold more information about each status, like turns remaining, etc.
    Charging(PokemonMove, Vec<crate::battle::FieldSlot>),
    Invulnerable(PokemonMove, Vec<crate::battle::FieldSlot>),
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

#[derive(Clone)]
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
        let gender_str = match self.gender {
            PokemonGender::Male => " (M)",
            PokemonGender::Female => " (F)",
            PokemonGender::Genderless => "",
        };
        
        let item_str = match &self.item {
            Item::None => String::new(),
            _ => format!(" @ {:?}", self.item),
        };

        let species_str_val = self.species.to_string();
        let mut speciesChars = species_str_val.chars();
        let species_str = match speciesChars.next() {
            None => String::new(),
            Some(f) => f.to_uppercase().collect::<String>() + speciesChars.as_str(),
        };


        writeln!(f, "{}{}{}", species_str, gender_str, item_str)?;
        writeln!(f, "Ability: {}", self.ability.to_string())?;
        
        if self.level != 100 {
            writeln!(f, "Level: {}", self.level)?;
        }
        
        writeln!(f, "Tera Type: {:?}", self.tera_type)?;
        
        let stat_names = ["HP", "Atk", "Def", "SpA", "SpD", "Spe"];
        
        let mut ev_strs = Vec::new();
        for i in 0..6 {
            if self.evs[i] > 0 {
                ev_strs.push(format!("{} {}", self.evs[i], stat_names[i]));
            }
        }
        if !ev_strs.is_empty() {
            writeln!(f, "EVs: {}", ev_strs.join(" / "))?;
        }
        
        let mut iv_strs = Vec::new();
        for i in 0..6 {
            if self.ivs[i] < 31 {
                iv_strs.push(format!("{} {}", self.ivs[i], stat_names[i]));
            }
        }
        if !iv_strs.is_empty() {
            writeln!(f, "IVs: {}", iv_strs.join(" / "))?;
        }
        
        writeln!(f, "{:?} Nature", self.nature)?;
        
        for (i, mov) in self.moves.iter().enumerate() {
            if let Some(m) = mov {
                writeln!(f, "- {} ({} PP)", m.to_string(), self.move_pp[i])?;
            }
        }
        
        write!(f, "Stats: {} HP / {} Atk / {} Def / {} SpA / {} SpD / {} Spe\n",
            self.stats[0], self.stats[1], self.stats[2], self.stats[3], self.stats[4], self.stats[5])?;

        Ok(())
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
        let gender = explicit_gender.unwrap_or_else(|| {
            dex_entry.map(|data| data.default_gender).unwrap_or(PokemonGender::Genderless)
        });

        // --- Parse remaining lines ---
        let mut ability: Option<Ability> = None;
        let mut level: u8 = 50;
        let mut tera_type = PokemonType::Normal;
        let mut evs = [0u8; 6]; // hp, atk, def, spa, spd, spe
        let mut ivs = [31u8; 6];
        let mut nature = Nature::Hardy;
        let mut moves: [Option<PokemonMove>; 4] = [None, None, None, None];
        let mut move_pp: [u8; 4] = [0, 0, 0, 0];
        let mut move_count = 0;

        for &line in &lines[1..] {
            if let Some(rest) = line.strip_prefix("Ability:") {
                ability = Some(Ability::from_str(rest.trim()));
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
                            // Convert from stat points to EVs if needed
                            let ev_value = if use_stat_points {
                                // Apply formula: EV(n) = ((n-4)/8)+1 to convert stat points to EVs
                                ((val as i16 * 8) - 4).max(0) as u8
                            } else {
                                // Normal EV format, use as-is
                                val
                            };
                            
                            match stat_name.trim() {
                                "HP"  => evs[0] = ev_value,
                                "Atk" => evs[1] = ev_value,
                                "Def" => evs[2] = ev_value,
                                "SpA" => evs[3] = ev_value,
                                "SpD" => evs[4] = ev_value,
                                "Spe" => evs[5] = ev_value,
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
                    let parsed_move = PokemonMove::from_str(move_str);
                    moves[move_count] = Some(parsed_move.clone());
                    
                    if let Some(move_data) = move_dex.get(&parsed_move) {
                        if parsed_move == PokemonMove::Protect {
                            move_pp[move_count] = 8;
                        } else {
                            match move_data.pp {
                                5 => move_pp[move_count] = 8,
                                10 => move_pp[move_count] = 12,
                                15 => move_pp[move_count] = 16,
                                pp if pp > 15 => move_pp[move_count] = 20,
                                pp => move_pp[move_count] = pp, // Fallback
                            }
                        }
                    }

                    move_count += 1;
                }
            }
        }

        // --- Look up dex data ---
        let (types, base_stats, weight_hg) = match pokemon_dex.get(&species_key) {
            Some(data) => (data.types.clone(), data.base_stats, data.weight),
            None => {
                eprintln!("Warning: '{}' not found in dex (key: '{:?}')", base_name, species_key);
                (vec![PokemonType::Normal], [100u16; 6], 0u16)
            }
        };

        // --- Compute actual stats from base stats, EVs, IVs, level, nature ---
        let stats = calc_stats_for_level(base_stats, ivs, evs, level, &nature);
        let hp = stats[0];

        let normalized_ability = ability.unwrap_or(Ability::Unknown(String::new()));
        let normalized_item_str = normalize_string(&item);
        let item_enum = Item::from_str(&item);
        let is_mega = dex_entry
            .map(|data| is_mega_dex_entry(&species_key, data))
            .unwrap_or(false);
        let mega_species = if is_mega {
            None
        } else {
            resolve_mega_species(&species_key, &normalized_item_str, pokemon_dex)
        };
        let mega_ability = mega_species
            .as_ref()
            .and_then(|key| pokemon_dex.get(key))
            .and_then(|data| data.primary_ability.clone());

        team.push(PokemonState {
            fainted: false,
            species: species_key,
            types,
            tera_type,
            is_tera: false,
            is_mega,
            has_mega_form: mega_species.is_some(),
            mega_species,
            mega_ability,
            stats,
            evs,
            ivs,
            move_pp,
            boosts: [0; 7],
            level,
            hp,
            moves,
            item: item_enum,
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

