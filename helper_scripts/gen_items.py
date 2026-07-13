import re
import os

with open('pokemon_info/showdownItems.txt', 'r', encoding='utf-8') as f:
    text = f.read()

# Collect item names and, per item, its Fling data (base power + optional
# status/volatile rider). Items are top-level blocks `id: {\n ... \n},` whose
# closing `},` sits at column 0; nested blocks (e.g. `fling: {`) close on an
# indented `},`, so a column-0 `\n},` reliably terminates the item block.
items = []
fling_power = {}   # ident -> base power (u16)
fling_effect = {}  # ident -> 'brn' | 'par' | 'psn' | 'tox' | 'flinch'
berries = set()    # idents flagged isBerry
z_crystals = set() # idents with a zMove (Z-Crystals)
plates = set()     # idents with onPlate (Arceus plates)
drives = set()     # idents with onDrive (Genesect drives)
memories = set()   # idents with onMemory (Silvally memories)

def ident_of(name):
    valid = ''.join(c for c in name if c.isalnum())
    if valid and valid[0].isdigit():
        valid = 'Num' + valid
    return valid

def fix_unicode_escapes(name):
    # Showdown's source data embeds names using JS-style `\uXXXX` escapes (no
    # braces) for non-ASCII characters. Rust string literals require braces
    # around the hex digits (`\u{XXXX}`) — a bare `\uXXXX` is a compile error.
    # No current item name is affected, but this mirrors gen_enums.py's fix in
    # case a future item name needs it. `ident_of` filters to alphanumeric
    # characters regardless of whether braces are present, so this is safe to
    # apply unconditionally.
    return re.sub(r'\\u([0-9a-fA-F]{4})', r'\\u{\1}', name)

for block in re.finditer(r'^\w+: \{\n(.*?)\n\},', text, re.DOTALL | re.MULTILINE):
    body = block.group(1)
    name_match = re.search(r'name:\s*"([^"]+)"', body)
    if not name_match:
        continue
    name = fix_unicode_escapes(name_match.group(1))
    ident = ident_of(name)
    items.append(name)

    is_berry = re.search(r'isBerry:\s*true', body) is not None
    if is_berry:
        berries.add(ident)
    if re.search(r'zMove:', body):
        z_crystals.add(ident)
    if re.search(r'onPlate:', body):
        plates.add(ident)
    if re.search(r'onDrive:', body):
        drives.add(ident)
    if re.search(r'onMemory:', body):
        memories.add(ident)

    fling_match = re.search(r'fling:\s*\{([^}]*)\}', body)
    if fling_match:
        fling_body = fling_match.group(1)
        bp_match = re.search(r'basePower:\s*(\d+)', fling_body)
        if bp_match:
            fling_power[ident] = int(bp_match.group(1))
        status_match = re.search(r"status:\s*'(\w+)'", fling_body)
        volatile_match = re.search(r"volatileStatus:\s*'(\w+)'", fling_body)
        if status_match:
            fling_effect[ident] = status_match.group(1)
        elif volatile_match:
            fling_effect[ident] = volatile_match.group(1)

# Berries have no explicit `fling` block in the data; the Showdown engine gives
# every Berry a default Fling base power of 10.
for ident in berries:
    fling_power.setdefault(ident, 10)

# Type-based Z-Crystals also carry `onPlate` to denote their type; they are not
# Plates. Z-Crystal classification takes precedence.
plates -= z_crystals

items = sorted(list(set(items)))

with open('poke_rust/src/data/item.rs', 'w', encoding='utf-8') as f:
    f.write('#[derive(Debug, Clone, PartialEq, Eq, Hash)]\n')
    f.write('pub enum Item {\n')
    for item in items:
        f.write(f'    {ident_of(item)},\n')
    f.write('    None,\n')
    f.write('    Unknown(String),\n')
    f.write('}\n\n')

    f.write('impl Item {\n')
    f.write('    #[allow(clippy::should_implement_trait)]\n')
    f.write('    pub fn from_str(s: &str) -> Self {\n')
    f.write('        let normalize = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect::<String>();\n')
    f.write('        let normalized = normalize(s);\n')
    f.write('        match normalized.as_str() {\n')
    for item in items:
        lower_ident = ''.join(c for c in item if c.isalnum()).lower()
        f.write(f'            "{lower_ident}" => Item::{ident_of(item)},\n')
    f.write('            "" => Item::None,\n')
    f.write('            "none" => Item::None,\n')
    f.write('            _ => Item::Unknown(s.to_string()),\n')
    f.write('        }\n')
    f.write('    }\n\n')

    # Fling base power for this item, or None if the item cannot be flung
    # (the move fails). Generated from each item's `fling.basePower` in the
    # Showdown item data.
    f.write('    /// Base power when thrown by Fling, or `None` if the item cannot be flung.\n')
    f.write('    pub fn fling_power(&self) -> Option<u16> {\n')
    f.write('        match self {\n')
    for item in items:
        ident = ident_of(item)
        if ident in fling_power:
            f.write(f'            Item::{ident} => Some({fling_power[ident]}),\n')
    f.write('            _ => None,\n')
    f.write('        }\n')
    f.write('    }\n\n')

    # Added effect inflicted on the target when this item is flung, as the raw
    # Showdown id ("brn", "par", "psn", "tox", "flinch"). `None` for items with
    # no rider (or whose rider is a callback, e.g. Mental/White Herb, handled
    # explicitly by the Fling code).
    f.write('    /// Status/volatile rider applied to the Fling target, as a Showdown id.\n')
    f.write('    pub fn fling_effect_id(&self) -> Option<&\'static str> {\n')
    f.write('        match self {\n')
    for item in items:
        ident = ident_of(item)
        if ident in fling_effect:
            f.write(f'            Item::{ident} => Some("{fling_effect[ident]}"),\n')
    f.write('            _ => None,\n')
    f.write('        }\n')
    f.write('    }\n\n')

    # Whether this item is a Berry (Showdown `isBerry`). Used by Bug Bite / Pluck,
    # Teatime, Fling, and other berry-aware mechanics.
    f.write('    /// Whether this item is a Berry.\n')
    f.write('    pub fn is_berry(&self) -> bool {\n')
    f.write('        matches!(self,\n')
    berry_idents = sorted(berries)
    if berry_idents:
        f.write('            ' + '\n            | '.join(f'Item::{i}' for i in berry_idents) + '\n')
    f.write('        )\n')
    f.write('    }\n')

    # Item categories that are "locked" to a particular species (Plates→Arceus,
    # Drives→Genesect, Memories→Silvally) or to no holder at all (Z-Crystals).
    # Used by item_cannot_be_transferred for Knock Off / Trick / Thief / etc.
    def write_category(method, idents, doc):
        f.write('\n')
        f.write(f'    /// {doc}\n')
        f.write(f'    pub fn {method}(&self) -> bool {{\n')
        f.write('        matches!(self,\n')
        s = sorted(idents)
        if s:
            f.write('            ' + '\n            | '.join(f'Item::{i}' for i in s) + '\n')
        else:
            f.write('            Item::None if false\n')
        f.write('        )\n')
        f.write('    }\n')

    write_category('is_z_crystal', z_crystals, 'Whether this item is a Z-Crystal.')
    write_category('is_plate', plates, 'Whether this item is a Plate (Arceus type item).')
    write_category('is_drive', drives, 'Whether this item is a Drive (Genesect type item).')
    write_category('is_memory', memories, 'Whether this item is a Memory (Silvally type item).')
    f.write('}\n')
