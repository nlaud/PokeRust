import re
import os

def fix_unicode_escapes(name):
    # Showdown's source data embeds names using JS-style `\uXXXX` escapes (no
    # braces) for non-ASCII characters, e.g. Farfetch'd as `Farfetch’d` and
    # Flabébé (NFD: e + combining acute) as `Flabébé`. Rust string
    # literals require braces around the hex digits (`\u{XXXX}`) — a bare
    # `\uXXXX` is a compile error. This only rewrites the escape's delimiters;
    # `normalize_ident`/`ident_of` filter to alphanumeric characters regardless
    # of whether braces are present, so enum variant names are unaffected.
    return re.sub(r'\\u([0-9a-fA-F]{4})', r'\\u{\1}', name)

def normalize_ident(name):
    ident = ''.join(c for c in name if c.isalnum())
    if not ident: return "Unknown"
    if ident[0].isdigit():
        ident = 'Num' + ident
    return ident

def generate_enum(filename, enum_name, type_name, items, default_variants=None):
    with open(f'src/data/{filename}.rs', 'w', encoding='utf-8') as f:
        f.write('#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]\n')
        f.write(f'pub enum {type_name} {{\n')
        for item in items:
            f.write(f'    {normalize_ident(item)},\n')
        if default_variants:
            for variant in default_variants:
                f.write(f'    {variant},\n')
        f.write('    Unknown(String),\n')
        f.write('}\n\n')

        f.write(f'impl {type_name} {{\n')
        f.write('    #[allow(clippy::should_implement_trait)]\n')
        f.write('    pub fn from_str(s: &str) -> Self {\n')
        f.write('        let normalize = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect::<String>();\n')
        f.write('        let normalized = normalize(s);\n')
        f.write('        match normalized.as_str() {\n')
        for item in items:
            ident = normalize_ident(item)
            lower_ident = ''.join(c for c in item if c.isalnum()).lower()
            f.write(f'            "{lower_ident}" => {type_name}::{ident},\n')
        if default_variants:
            for variant in default_variants:
                lower_ident = variant.lower()
                f.write(f'            "{lower_ident}" => {type_name}::{variant},\n')
                if variant == 'None':
                    f.write(f'            "" => {type_name}::{variant},\n')
        
        f.write(f'            _ => {type_name}::Unknown(s.to_string()),\n')
        f.write('        }\n')
        f.write('    }\n')

        f.write('    #[allow(clippy::inherent_to_string)]\n')
        f.write('    pub fn to_string(&self) -> String {\n')
        f.write('        match self {\n')
        for item in items:
            ident = normalize_ident(item)
            f.write(f'            {type_name}::{ident} => "{item}".to_string(),\n')
        if default_variants:
            for variant in default_variants:
                f.write(f'            {type_name}::{variant} => "{variant}".to_string(),\n')
        f.write(f'            {type_name}::Unknown(s) => s.clone(),\n')
        f.write('        }\n')
        f.write('    }\n')
        f.write('}\n')

species_list = []
ability_list = []
move_list = []

with open('../pokemon_info/showdownDex.txt', 'r', encoding='utf-8') as f:
    text = f.read()

for match in re.finditer(r'name:\s*"([^"]+)"', text):
    species_list.append(fix_unicode_escapes(match.group(1)))

# Extract abilities
# Abilities might appear as abilities: {0: "Blaze", 1: "Solar Power", H: "Chlorophyll", S: "Custom"}
for match in re.finditer(r'abilities:\s*\{([^}]+)\}', text):
    abi_block = match.group(1)
    for abi_match in re.finditer(r'"([^"]+)"', abi_block):
        ability_list.append(fix_unicode_escapes(abi_match.group(1)))

with open('../pokemon_info/showdownMoves.txt', 'r', encoding='utf-8') as f:
    text = f.read()

for match in re.finditer(r'name:\s*"([^"]+)"', text):
    move_list.append(fix_unicode_escapes(match.group(1)))

species_list = sorted(list(set(species_list)))
ability_list = sorted(list(set(ability_list)))
move_list = sorted(list(set(move_list)))

generate_enum('species', 'species', 'Species', species_list, ['None'])
generate_enum('ability', 'ability', 'Ability', ability_list, ['None'])
generate_enum('pokemon_move', 'poke_move', 'PokemonMove', move_list, ['None'])
print("Generated species.rs, ability.rs, pokemon_move.rs")
