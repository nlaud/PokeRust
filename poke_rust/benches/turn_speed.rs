//! Turn-resolution speed benchmark: times a single attack-turn resolution
//! across enumerate/sample mode × damage rolls × crit branching, in singles
//! and doubles.
//!
//! Run from `poke_rust/`:
//! ```sh
//! cargo bench --bench turn_speed
//! ```
//!
//! Scenarios (fixed Rain teams so runs are comparable over time):
//! - singles: Aerodactyl Rock Slide vs Pelipper Hurricane
//! - doubles: Rock Slide + Heat Wave spreads vs Hurricane + Draco Meteor —
//!   the turn shape that makes full enumeration explode
//!
//! Enumeration rows that are known to be intractable (doubles at high roll
//! counts) are skipped; see the allowlist below. Recorded results live in
//! `benches/RESULTS.md`.

use std::time::Instant;

use poke_rust::simulator::{sample_turn, simulate_turn, team_preview_state_from_team_strings};
use poke_rust::state::battle::{
    AttackCommand, BattleCommand, FieldSlot, MatchState, Player, PlayerCommand, TeamPreviewCommand,
};

const P1_TEAM: &str = "Aerodactyl @ Aerodactylite
Ability: Unnerve
Level: 50
EVs: 12 HP / 12 Atk / 9 Def / 1 SpD / 32 Spe
Jolly Nature
- Rock Slide
- Dual Wingbeat
- Tailwind
- Protect

Charizard @ Charizardite Y
Ability: Blaze
Level: 50
EVs: 32 HP / 10 Def / 11 SpA / 13 Spe
Modest Nature
- Heat Wave
- Weather Ball
- Solar Beam
- Protect

Basculegion (M) @ Focus Sash
Ability: Adaptability
Level: 50
EVs: 2 HP / 32 Atk / 32 Spe
Adamant Nature
- Liquidation
- Last Respects
- Aqua Jet
- Protect

Garchomp @ Choice Scarf
Ability: Rough Skin
Level: 50
EVs: 18 HP / 20 Atk / 1 SpD / 27 Spe
Adamant Nature
- Dragon Claw
- Earthquake
- Rock Slide
- Stomping Tantrum";

const P2_TEAM: &str = "Pelipper @ Focus Sash
Ability: Drizzle
Level: 50
EVs: 1 HP / 32 SpA / 32 Spe
Modest Nature
- Hurricane
- Weather Ball
- Tailwind
- Wide Guard

Dragonite @ Dragoninite
Ability: Multiscale
Level: 50
EVs: 2 HP / 32 SpA / 32 Spe
Modest Nature
- Draco Meteor
- Fire Blast
- Ice Beam
- Extreme Speed

Archaludon @ Leftovers
Ability: Stamina
Level: 50
EVs: 28 HP / 1 Def / 6 SpA / 23 SpD / 8 Spe
Modest Nature
- Thunderbolt
- Flash Cannon
- Electro Shot
- Dragon Pulse

Incineroar @ Sitrus Berry
Ability: Intimidate
Level: 50
EVs: 32 HP / 16 Def / 17 SpD
Careful Nature
- Protect
- Parting Shot
- Fake Out
- Flare Blitz";

fn main() {
    poke_rust::VERBOSITY.set(0).ok();
    let pokemon_dex = poke_rust::state::dex_data::parse_pokemon_dex("../pokemon_info/showdownDex.txt");
    let move_dex = poke_rust::state::dex_data::parse_move_dex("../pokemon_info/showdownMoves.txt");

    let make_state = |active: usize, brought: usize| -> MatchState {
        let preview = team_preview_state_from_team_strings(
            P1_TEAM, P2_TEAM, &pokemon_dex, &move_dex, active as u8, brought as u8, true,
        );
        let pv = PlayerCommand::TeamPreview(TeamPreviewCommand {
            active_indices: (0..active).collect(),
            back_indices: (active..brought).collect(),
        });
        simulate_turn(&MatchState::TeamPreviewState(preview), &pv, &pv, &move_dex, &pokemon_dex, false, 1, None)
            .into_iter()
            .max_by(|a, b| a.2.partial_cmp(&b.2).unwrap())
            .unwrap()
            .0
    };

    let slot = |player, slot_index| FieldSlot { player, slot_index };
    let atk = |move_slot, target| {
        BattleCommand::Attack(AttackCommand { move_slot, target, terastallize: false, mega_evolve: false })
    };

    // Singles: Aerodactyl Rock Slide vs Pelipper Hurricane (rain: Hurricane can't miss).
    let singles = make_state(1, 3);
    let singles_cmds = (
        PlayerCommand::Battle(vec![atk(0, Some(slot(Player::P2, 0)))]),
        PlayerCommand::Battle(vec![atk(0, Some(slot(Player::P1, 0)))]),
    );

    // Doubles: Rock Slide + Heat Wave spreads vs Hurricane -> Charizard, Draco Meteor -> Aerodactyl.
    // This turn shape exceeded 15 GB under full enumeration at 16 rolls + crit.
    let doubles = make_state(2, 4);
    let doubles_cmds = (
        PlayerCommand::Battle(vec![atk(0, None), atk(0, None)]),
        PlayerCommand::Battle(vec![
            atk(0, Some(slot(Player::P1, 1))),
            atk(0, Some(slot(Player::P1, 0))),
        ]),
    );

    let rolls_grid = [1u8, 2, 4, 8, 16];
    // Enumeration allowlist per scenario: (rolls, crit) combos that stay tractable.
    // Doubles branch counts grow ~16-20x per roll doubling; anything beyond this
    // list runs for minutes and/or exhausts memory.
    let doubles_enum_ok = |rolls: u8, crit: bool| matches!((rolls, crit), (1, _) | (2, _) | (4, false));

    println!("{:<8} {:<10} {:>5} {:>5} {:>12} {:>10}", "scenario", "mode", "rolls", "crit", "time", "branches");

    for (name, state, (p1, p2), enum_ok) in [
        ("singles", &singles, &singles_cmds, &(|_, _| true) as &dyn Fn(u8, bool) -> bool),
        ("doubles", &doubles, &doubles_cmds, &doubles_enum_ok as &dyn Fn(u8, bool) -> bool),
    ] {
        for crit in [false, true] {
            for rolls in rolls_grid {
                // Full enumeration: one run for slow configs, a few for fast ones.
                if enum_ok(rolls, crit) {
                    let mut iters = 0u32;
                    let mut branches = 0usize;
                    let start = Instant::now();
                    while iters < 5 && (iters == 0 || start.elapsed().as_secs_f64() < 0.4) {
                        branches = simulate_turn(state, p1, p2, &move_dex, &pokemon_dex, crit, rolls, None).len();
                        iters += 1;
                    }
                    let per = start.elapsed().as_secs_f64() / iters as f64;
                    println!("{:<8} {:<10} {:>5} {:>5} {:>12} {:>10}", name, "enumerate", rolls, crit, fmt_time(per), branches);
                } else {
                    println!("{:<8} {:<10} {:>5} {:>5} {:>12} {:>10}", name, "enumerate", rolls, crit, "skipped", "-");
                }

                // Sample mode: average over many runs (each run is a different trajectory).
                let mut iters = 0u32;
                let start = Instant::now();
                while iters < 10 || start.elapsed().as_secs_f64() < 0.4 {
                    let _ = sample_turn(state, p1, p2, &move_dex, &pokemon_dex, crit, rolls, None);
                    iters += 1;
                }
                let per = start.elapsed().as_secs_f64() / iters as f64;
                println!("{:<8} {:<10} {:>5} {:>5} {:>12} {:>10}", name, "sample", rolls, crit, fmt_time(per), 1);
            }
        }
    }
}

fn fmt_time(seconds: f64) -> String {
    if seconds >= 1.0 {
        format!("{:.2} s", seconds)
    } else if seconds >= 0.001 {
        format!("{:.2} ms", seconds * 1000.0)
    } else {
        format!("{:.0} µs", seconds * 1_000_000.0)
    }
}
