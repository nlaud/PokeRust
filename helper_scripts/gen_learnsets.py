"""Regenerates pokemon_info/showdownLearnsets.txt from the authoritative Pokemon
Champions mod source (pokemon_info/championsLearnsets.ts, downloaded from
https://raw.githubusercontent.com/smogon/pokemon-showdown/master/data/mods/champions/learnsets.ts).

The Champions mod file only carries an override/patch table (~232 species), and every
move entry in it is already flattened to a single "9M" tag (Champions doesn't track
level/egg/tutor provenance separately). This script's only real job beyond reformatting
is prevo-chain inheritance: an evolved species' Champions entry lists only ITS OWN
moves, not moves inherited from its pre-evolution(s) (Bulbapedia/Showdown convention:
a Pokemon can always use any move a member of its own evolution line could learn at an
earlier stage). Without this, an evolved mon whose pre-evolution's Champions entry has
a move it doesn't itself re-list would be wrongly flagged illegal.

Run from the repo root:
    python helper_scripts/gen_learnsets.py
"""

import re


CHAMPIONS_SRC = "pokemon_info/championsLearnsets.ts"
DEX_SRC = "pokemon_info/showdownDex.txt"
OUT_PATH = "pokemon_info/showdownLearnsets.txt"


def normalize_id(name):
    """Showdown's own ID convention: lowercase alphanumeric only."""
    return "".join(c for c in name if c.isalnum()).lower()


def parse_champions_learnsets(path):
    """Returns {species_id: set(move_id)}. Each top-level `speciesid: { learnset: {
    moveid: [...], ... }, },` block is parsed by brace depth, mirroring split_entries
    in state/dex_data.rs (this file has the same nested-brace shape)."""
    with open(path, encoding="utf-8") as f:
        content = f.read()

    result = {}
    # Top-level entries: `\tspeciesid: {` at one-tab indent, matching this file's
    # formatting (mirrors showdownLearnsets.txt/showdownDex.txt's own convention).
    entry_re = re.compile(r"^\t([a-zA-Z0-9]+): \{\n(.*?)\n\t\},$", re.M | re.S)
    move_re = re.compile(r'^\t\t\t([a-zA-Z0-9]+): \[')

    for m in entry_re.finditer(content):
        species_id = m.group(1)
        block = m.group(2)
        in_learnset = False
        moves = set()
        for line in block.split("\n"):
            stripped = line.strip()
            if not in_learnset:
                if stripped.startswith("learnset:"):
                    in_learnset = True
                continue
            if stripped == "},":
                break
            mm = move_re.match(line)
            if mm:
                moves.add(mm.group(1))
        if moves:
            result[species_id] = moves

    return result


def parse_prevo_chains(path):
    """Returns {species_id: prevo_species_id_or_None} and {species_id: base_species_id_or_None},
    both keyed/valued by normalized (lowercase-alphanumeric) Showdown id, matching the
    learnset table's own key format."""
    with open(path, encoding="utf-8") as f:
        content = f.read()

    prevo = {}
    base_species = {}
    entry_re = re.compile(r"^([a-zA-Z0-9]+): \{\n(.*?)\n\},$", re.M | re.S)
    for m in entry_re.finditer(content):
        species_id = m.group(1)
        block = m.group(2)
        pv = re.search(r'prevo: "([^"]+)"', block)
        bs = re.search(r'baseSpecies: "([^"]+)"', block)
        if pv:
            prevo[species_id] = normalize_id(pv.group(1))
        if bs:
            base_species[species_id] = normalize_id(bs.group(1))
    return prevo, base_species


def resolve_ancestors(species_id, prevo, base_species, seen=None):
    """All prevo-chain ancestors AND the baseSpecies (if this is an alternate forme),
    transitively. `seen` guards against cycles (shouldn't occur in real data, but a
    generator should never infinite-loop on malformed input)."""
    if seen is None:
        seen = set()
    ancestors = []
    for related in (prevo.get(species_id), base_species.get(species_id)):
        if related and related != species_id and related not in seen:
            seen.add(related)
            ancestors.append(related)
            ancestors.extend(resolve_ancestors(related, prevo, base_species, seen))
    return ancestors


def main():
    learnsets = parse_champions_learnsets(CHAMPIONS_SRC)
    prevo, base_species = parse_prevo_chains(DEX_SRC)

    unioned = {}
    for species_id, moves in learnsets.items():
        full = set(moves)
        for ancestor in resolve_ancestors(species_id, prevo, base_species):
            full |= learnsets.get(ancestor, set())
        unioned[species_id] = full

    added_species = sorted(unioned.keys() - learnsets.keys())
    inherited_count = sum(
        len(unioned[s]) - len(learnsets[s]) for s in learnsets if len(unioned[s]) > len(learnsets[s])
    )
    print(f"Species with data: {len(unioned)}")
    print(f"Total inherited-move additions from prevo/baseSpecies chains: {inherited_count}")
    if added_species:
        print(f"WARNING: species appeared via union that weren't in the source file: {added_species}")

    with open(OUT_PATH, "w", encoding="utf-8", newline="\n") as f:
        for species_id in sorted(unioned.keys()):
            moves = unioned[species_id]
            if not moves:
                continue
            f.write(f"{species_id}: {{\n")
            f.write("\tlearnset: {\n")
            for move_id in sorted(moves):
                f.write(f'\t\t{move_id}: ["9M"],\n')
            f.write("\t},\n")
            f.write("},\n")

    print(f"Wrote {OUT_PATH}")


if __name__ == "__main__":
    main()
