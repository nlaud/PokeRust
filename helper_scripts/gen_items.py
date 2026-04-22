import re
import os

items = []
with open('pokemon_info/showdownItems.txt', 'r', encoding='utf-8') as f:
    text = f.read()

for match in re.finditer(r'name:\s*"([^"]+)"', text):
    items.append(match.group(1))

items = sorted(list(set(items)))
with open('poke_rust/src/item.rs', 'w', encoding='utf-8') as f:
    f.write('#[derive(Debug, Clone, PartialEq, Eq, Hash)]\n')
    f.write('pub enum Item {\n')
    for item in items:
        valid_ident = ''.join(c for c in item if c.isalnum())
        if valid_ident[0].isdigit():
            valid_ident = 'Num' + valid_ident
        f.write(f'    {valid_ident},\n')
    f.write('    None,\n')
    f.write('    Unknown(String),\n')
    f.write('}\n\n')

    f.write('impl Item {\n')
    f.write('    pub fn from_str(s: &str) -> Self {\n')
    f.write('        let normalize = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect::<String>();\n')
    f.write('        let normalized = normalize(s);\n')
    f.write('        match normalized.as_str() {\n')
    for item in items:
        valid_ident = ''.join(c for c in item if c.isalnum())
        if valid_ident[0].isdigit():
            valid_ident = 'Num' + valid_ident
        lower_ident = ''.join(c for c in item if c.isalnum()).lower()
        f.write(f'            "{lower_ident}" => Item::{valid_ident},\n')
    f.write('            "" => Item::None,\n')
    f.write('            "none" => Item::None,\n')
    f.write('            _ => Item::Unknown(s.to_string()),\n')
    f.write('        }\n')
    f.write('    }\n')
    f.write('}\n')
