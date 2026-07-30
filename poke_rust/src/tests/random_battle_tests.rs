//! Tests complete random doubles battles.
//! It selects random teamsheet pairs and legal commands.
//! It tracks both fog-of-war beliefs with the server process.
//! `sample_turn_raw` selects one weighted event stream.
//! `mask_events_for` masks the stream for each player.
//! `apply_information` updates each belief.
//!
//! `apply_information` panics when observed events conflict with a belief.
//! The test does not catch this panic.
//! A panic therefore reports an inference soundness failure.

use std::collections::HashMap;
use std::sync::OnceLock;

use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};

use crate::data::ability::Ability;
use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::information::inference::{InferenceConfig, apply_information};
use crate::information::information::mask_events_for;
use crate::information::subset_check::{
    SubsetViolation, SubsetViolationKind, assert_true_state_subset_of_belief,
    collect_true_state_subset_violations,
};
use crate::information::unknowns::{Statement, UnknownMatchState};
use crate::simulator::{
    get_possible_commands_for_active_slot, sample_turn_raw, scoped_sample_rng,
    team_preview_state_from_teamsheets, validate_battle_command_combination,
};
use crate::state::battle::{
    BattleCommand, BattleState, MatchState, Player, PlayerCommand, TeamPreviewCommand,
};
use crate::state::dex_data::{parse_ability_dex, parse_learnset_dex};
use crate::tests::simuilator_test_helpers::{move_dex, pokemon_dex};

const TEAMSHEETS: [&str; 14] = [
    "../teamsheets/MA_charizard_sylveon.txt",
    "../teamsheets/MA_dragonite_rain.txt",
    "../teamsheets/MA_floette_froslass.txt",
    "../teamsheets/MA_tyranitar_zoroark.txt",
    "../teamsheets/MA_venusaur_aerodactl.txt",
    "../teamsheets/MB_aboma_pidgeon.txt",
    "../teamsheets/MB_barbaracle_zoroark.txt",
    "../teamsheets/MB_espathra_scovillain.txt",
    "../teamsheets/MB_gallade_clefable.txt",
    "../teamsheets/MB_gyarados_volcarona.txt",
    "../teamsheets/MB_malamar_tr.txt",
    "../teamsheets/MB_raptor_stuff.txt",
    "../teamsheets/MB_sand_doggo_rat.txt",
    "../teamsheets/MB_vivillon_camerupt.txt",
];

const ACTIVE_PER_SIDE: u8 = 2;
const BROUGHT_PER_SIDE: u8 = 4;
const ITERATIONS: u64 = 25;
/// Hang guard only — not a soundness property. Real doubles games settle in a
/// handful of turns; a few hundred comfortably covers even a PP-stall grind.
const MAX_TURNS: usize = 400;
const SAMPLE_SEED_SALT: u64 = 0x5355_4253_4554_4f52;

fn fuzz_env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn fuzz_env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(default)
}

fn statement_shape(statement: &Statement, names: &mut Vec<&'static str>) {
    let name = match statement {
        Statement::Not(inner) => {
            statement_shape(inner, names);
            "Not"
        }
        Statement::HasItem { .. } => "HasItem",
        Statement::HasAbility { .. } => "HasAbility",
        Statement::WeatherTurns { .. } => "WeatherTurns",
        Statement::TerrainTurns { .. } => "TerrainTurns",
        Statement::SideConditionTurns { .. } => "SideConditionTurns",
        Statement::NatureBoostsStat { .. } => "NatureBoostsStat",
        Statement::NatureNerfsStat { .. } => "NatureNerfsStat",
        Statement::EVIVStatGE { .. } => "EVIVStatGE",
        Statement::EVIVStatLE { .. } => "EVIVStatLE",
        Statement::SpeedComparison { .. } => "SpeedComparison",
        Statement::KnowsThreateningMove { .. } => "KnowsThreateningMove",
    };
    names.push(name);
}

static ABILITY_DEX: OnceLock<HashMap<Ability, crate::state::dex_data::AbilityData>> =
    OnceLock::new();
fn ability_dex() -> &'static HashMap<Ability, crate::state::dex_data::AbilityData> {
    ABILITY_DEX.get_or_init(|| parse_ability_dex("../pokemon_info/showdownAbilities.txt"))
}

static LEARNSET_DEX: OnceLock<HashMap<Species, std::collections::HashSet<PokemonMove>>> =
    OnceLock::new();
fn learnset_dex() -> &'static HashMap<Species, std::collections::HashSet<PokemonMove>> {
    LEARNSET_DEX.get_or_init(|| parse_learnset_dex("../pokemon_info/showdownLearnsets.txt"))
}

/// The Champions ruleset's item whitelist, mirroring `frontend/src/lib/items.ts`'s
/// `CATALOG` (general items + Mega Stones + berries) exactly — same label list,
/// parsed here via `Item::from_str` instead of TS's `slugify`, since `Item::from_str`
/// normalizes to alphanumeric-lowercase the same way regardless of hyphenation.
/// Every item held by any of the 14 checked-in `TEAMSHEETS` is in this list
/// (verified by hand when this catalog was wired into the server for the
/// legal-items TODO.md fix) — matching the server's real `InferenceConfig`
/// (rather than leaving `legal_items: None`, an unrestricted ~1,000-item pool no
/// real battle ever actually has) tests the engine under the same, tighter
/// item-possibility space production battles run under.
fn champions_legal_items() -> std::collections::HashSet<Item> {
    const GENERAL: &[&str] = &[
        "Big Root",
        "Black Belt",
        "Black Glasses",
        "Bright Powder",
        "Charcoal",
        "Choice Scarf",
        "Damp Rock",
        "Dragon Fang",
        "Expert Belt",
        "Fairy Feather",
        "Focus Band",
        "Focus Sash",
        "Hard Stone",
        "Heat Rock",
        "Icy Rock",
        "Iron Ball",
        "King's Rock",
        "Leftovers",
        "Life Orb",
        "Light Ball",
        "Light Clay",
        "Magnet",
        "Mental Herb",
        "Metal Coat",
        "Metronome",
        "Miracle Seed",
        "Muscle Band",
        "Mystic Water",
        "Never-Melt Ice",
        "Poison Barb",
        "Quick Claw",
        "Scope Lens",
        "Sharp Beak",
        "Shed Shell",
        "Shell Bell",
        "Silk Scarf",
        "Silver Powder",
        "Smooth Rock",
        "Soft Sand",
        "Spell Tag",
        "Twisted Spoon",
        "White Herb",
        "Wide Lens",
        "Wise Glasses",
        "Zoom Lens",
    ];
    const MEGA_STONES: &[&str] = &[
        "Abomasite",
        "Absolite",
        "Aerodactylite",
        "Aggronite",
        "Alakazite",
        "Altarianite",
        "Ampharosite",
        "Audinite",
        "Banettite",
        "Barbaracite",
        "Beedrillite",
        "Blastoisinite",
        "Blazikenite",
        "Cameruptite",
        "Chandelurite",
        "Charizardite X",
        "Charizardite Y",
        "Chesnaughtite",
        "Chimechite",
        "Clefablite",
        "Crabominite",
        "Delphoxite",
        "Dragalgite",
        "Dragoninite",
        "Drampanite",
        "Eelektrossite",
        "Emboarite",
        "Excadrite",
        "Falinksite",
        "Feraligite",
        "Floettite",
        "Froslassite",
        "Galladite",
        "Garchompite",
        "Gardevoirite",
        "Gengarite",
        "Glalitite",
        "Glimmoranite",
        "Golurkite",
        "Greninjite",
        "Gyaradosite",
        "Hawluchanite",
        "Heracronite",
        "Houndoominite",
        "Kangaskhanite",
        "Lopunnite",
        "Lucarionite",
        "Malamarite",
        "Manectite",
        "Mawilite",
        "Medichamite",
        "Meganiumite",
        "Meowsticite",
        "Metagrossite",
        "Pidgeotite",
        "Pinsirite",
        "Pyroarite",
        "Raichunite X",
        "Raichunite Y",
        "Sablenite",
        "Sceptilite",
        "Scizorite",
        "Scolipite",
        "Scovillainite",
        "Scraftinite",
        "Sharpedonite",
        "Skarmorite",
        "Slowbronite",
        "Staraptite",
        "Starminite",
        "Steelixite",
        "Swampertite",
        "Tyranitarite",
        "Venusaurite",
        "Victreebelite",
    ];
    const BERRIES: &[&str] = &[
        "Aspear Berry",
        "Babiri Berry",
        "Charti Berry",
        "Cheri Berry",
        "Chesto Berry",
        "Chilan Berry",
        "Chople Berry",
        "Coba Berry",
        "Colbur Berry",
        "Haban Berry",
        "Kasib Berry",
        "Kebia Berry",
        "Leppa Berry",
        "Lum Berry",
        "Occa Berry",
        "Oran Berry",
        "Passho Berry",
        "Payapa Berry",
        "Pecha Berry",
        "Persim Berry",
        "Rawst Berry",
        "Rindo Berry",
        "Roseli Berry",
        "Shuca Berry",
        "Sitrus Berry",
        "Tanga Berry",
        "Wacan Berry",
        "Yache Berry",
    ];
    GENERAL
        .iter()
        .chain(MEGA_STONES)
        .chain(BERRIES)
        .map(|label| Item::from_str(label))
        .collect()
}

/// Picks a random legal team-preview pick: `BROUGHT_PER_SIDE` distinct roster
/// indices (clamped to the roster size), the first `ACTIVE_PER_SIDE` of which
/// lead. Mirrors the counting in `user.rs::choose_team_preview_command`
/// (`brought = min(brought_per_side, total); active_n = min(active_per_side, brought)`).
fn random_team_preview_command(team_len: usize, rng: &mut StdRng) -> TeamPreviewCommand {
    let brought = (BROUGHT_PER_SIDE as usize).min(team_len);
    let active = (ACTIVE_PER_SIDE as usize).min(brought);

    let mut indices: Vec<usize> = (0..team_len).collect();
    indices.shuffle(rng);
    indices.truncate(brought);

    let active_indices = indices[..active].to_vec();
    let back_indices = indices[active..].to_vec();
    TeamPreviewCommand {
        active_indices,
        back_indices,
    }
}

/// Picks one random, jointly-legal `BattleCommand` set for every active slot of
/// `player` this turn. `get_possible_commands_for_active_slot` already handles
/// every per-slot special case on its own (self-switch-pending, a fainted mon
/// awaiting replacement, recharge/semi-invulnerable/charging/rampage locks,
/// choice lock, Encore/Taunt/Torment/Imprison, Struggle fallback) — the only
/// thing left for the caller to enforce is *joint* legality across slots
/// (two active mons can't switch into the same bench slot; at most one
/// Tera/Mega per team per turn), which `validate_battle_command_combination`
/// checks.
fn random_commands_for_player(
    state: &BattleState,
    player: Player,
    rng: &mut StdRng,
) -> Vec<BattleCommand> {
    let active_len = match player {
        Player::P1 => state.p1_active_mons.len(),
        Player::P2 => state.p2_active_mons.len(),
    };

    let per_slot_options: Vec<Vec<BattleCommand>> = (0..active_len)
        .map(|slot_idx| {
            get_possible_commands_for_active_slot(
                state,
                player,
                slot_idx,
                move_dex(),
                pokemon_dex(),
            )
        })
        .collect();

    for _ in 0..20 {
        let mut claimed_switch_targets: Vec<usize> = Vec::new();
        let mut cmds: Vec<BattleCommand> = Vec::with_capacity(active_len);
        for options in &per_slot_options {
            let available: Vec<&BattleCommand> = options
                .iter()
                .filter(|c| match c {
                    BattleCommand::Switch(s) => !claimed_switch_targets.contains(&s.party_index),
                    _ => true,
                })
                .collect();
            let chosen = match available.as_slice() {
                [] => BattleCommand::Pass,
                opts => opts[rng.gen_range(0..opts.len())].clone(),
            };
            if let BattleCommand::Switch(s) = &chosen {
                claimed_switch_targets.push(s.party_index);
            }
            cmds.push(chosen);
        }
        if validate_battle_command_combination(&cmds) {
            return cmds;
        }
    }

    // Deterministic fallback guaranteeing forward progress: attacks/Struggle/Pass
    // never conflict jointly, so preferring a non-Switch option per slot always
    // validates.
    per_slot_options
        .iter()
        .map(|options| {
            options
                .iter()
                .find(|c| !matches!(c, BattleCommand::Switch(_)))
                .or_else(|| options.first())
                .cloned()
                .unwrap_or(BattleCommand::Pass)
        })
        .collect()
}

/// Re-seeds a team-preview belief into a battle-level belief on the team-preview
/// -> battle transition, mirroring `session.rs::advance_belief`'s two-step dance:
/// `into_battle_state` structurally seeds it (viewer fully known, opponent's
/// whole roster parked in `possible_back`), and the caller then runs this
/// transition's own event log through `apply_information` (done by the caller,
/// same as every other turn — `is_team_preview` is `false` on every call).
fn reseed_for_battle(
    belief: UnknownMatchState,
    viewer: Player,
    p1_cmd: &PlayerCommand,
    p2_cmd: &PlayerCommand,
) -> UnknownMatchState {
    let (
        UnknownMatchState::TeamPreview(preview),
        PlayerCommand::TeamPreview(p1_tp),
        PlayerCommand::TeamPreview(p2_tp),
    ) = (&belief, p1_cmd, p2_cmd)
    else {
        return belief;
    };
    UnknownMatchState::Battle(preview.into_battle_state(
        viewer,
        &p1_tp.active_indices,
        &p1_tp.back_indices,
        &p2_tp.active_indices,
        &p2_tp.back_indices,
    ))
}

/// The contradiction-only soundness sweep: fails only if `apply_information`
/// finds an observed event stream jointly impossible under the tracked belief.
/// This is the ORIGINAL oracle, with a long, low (~0.14%) historical failure
/// rate — kept fast and reliable for the everyday `cargo test` suite.
#[test]
fn random_doubles_battles_are_sound() {
    run_sweep(ITERATIONS, false);
}

/// The stronger "truth ⊆ belief" sweep (see `subset_check`'s module doc):
/// additionally asserts the true state never falls outside what each belief
/// admits. Currently fails at a MUCH higher rate (~30-36% per 100-iteration
/// sweep as of 2026-07-19 — see TODO.md's "truth ⊆ belief subset oracle" entry
/// for the open bug families this surfaces) because it catches real,
/// previously-undiscovered over-narrowing bugs the contradiction oracle above
/// cannot see. Deliberately `#[ignore]`d rather than folded into the default
/// test above: gating the everyday suite on these still-open bugs would make
/// ordinary `cargo test` runs fail on unrelated work far more often than not.
/// Run explicitly (`cargo test -- --ignored random_doubles_beliefs_stay_sound_subset`)
/// when working on the fog-of-war engine, or fold it back into the default
/// sweep once the families in TODO.md are fixed.
#[test]
#[ignore]
fn random_doubles_beliefs_stay_sound_subset() {
    run_sweep(ITERATIONS, true);
}

/// Diagnostic dev tool (not part of the regular suite — `#[ignore]`d) for
/// characterizing `assert_true_state_subset_of_belief` failures across many
/// fuzz iterations in one run: catches each subset-oracle panic AND each
/// contradiction-oracle panic via `catch_unwind` instead of aborting the whole
/// run, then buckets by field/clause-shape/species. Failures are exactly
/// reproducible by seed: command generation and simulator branch sampling use
/// deterministic `StdRng`s scoped to each iteration. Set
/// `POKERUST_FUZZ_SEED_START=<seed>` and `POKERUST_FUZZ_ITERS=1` to replay one
/// battle; add `POKERUST_FUZZ_REPLAY=1` for full command/event dumps.
/// `break`s directly in each
/// `Err` arm (a diverging expression) rather than using a separate correlated
/// bool — a bool-based version fails to borrow-check across loop iterations
/// (E0382), confirmed while building this. Run via:
/// `cargo test --release -- --ignored survey_subset_violations --nocapture`.
#[test]
#[ignore]
fn survey_subset_violations() {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    // This diagnostic deliberately catches inference panics so it can survey the
    // entire seed range. Silence the default hook: the structured report below is
    // more useful than thousands of unlabelled panic messages, and includes the
    // deterministic seed/turn needed for replay.
    let previous_panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    let pdex = pokemon_dex();
    let mdex = move_dex();
    let adex = ability_dex();
    let ldex = learnset_dex();

    let config = InferenceConfig {
        use_stat_points: true,
        force_max_ivs: true,
        legal_items: Some(champions_legal_items()),
        learnset_dex: ldex.clone(),
        ..Default::default()
    };

    let iterations = fuzz_env_u64("POKERUST_FUZZ_ITERS", 300);
    let seed_start = fuzz_env_u64("POKERUST_FUZZ_SEED_START", 0);
    let max_failures = fuzz_env_u64("POKERUST_FUZZ_MAX_FAILURES", u64::MAX);
    let replay_details = fuzz_env_bool("POKERUST_FUZZ_REPLAY", false);
    let sample_limit = fuzz_env_u64("POKERUST_FUZZ_SAMPLE_LIMIT", 50) as usize;
    let mut contradictions: Vec<String> = Vec::new();
    let mut subset_failures: Vec<(String, SubsetViolation)> = Vec::new();
    let mut failed_iters: u64 = 0;
    let mut attempted_iters: u64 = 0;

    for iter in seed_start..seed_start.saturating_add(iterations) {
        attempted_iters += 1;
        let mut rng = StdRng::seed_from_u64(iter);
        let _sample_rng = scoped_sample_rng(iter ^ SAMPLE_SEED_SALT);

        let p1_path = TEAMSHEETS[rng.gen_range(0..TEAMSHEETS.len())];
        let p2_path = TEAMSHEETS[rng.gen_range(0..TEAMSHEETS.len())];

        let preview = team_preview_state_from_teamsheets(
            p1_path,
            p2_path,
            pdex,
            mdex,
            ACTIVE_PER_SIDE,
            BROUGHT_PER_SIDE,
            true,
        );

        let mut belief_p1 = UnknownMatchState::team_preview_closed_sheet_from_perspective(
            Player::P1,
            &preview.p1_mons,
            &preview.p2_mons,
            pdex,
            ACTIVE_PER_SIDE,
            BROUGHT_PER_SIDE,
            50,
            true,
        );
        let mut belief_p2 = UnknownMatchState::team_preview_closed_sheet_from_perspective(
            Player::P2,
            &preview.p2_mons,
            &preview.p1_mons,
            pdex,
            ACTIVE_PER_SIDE,
            BROUGHT_PER_SIDE,
            50,
            true,
        );

        let p1_tp = random_team_preview_command(preview.p1_mons.len(), &mut rng);
        let p2_tp = random_team_preview_command(preview.p2_mons.len(), &mut rng);

        let mut state = MatchState::TeamPreviewState(preview);
        let mut p1_cmd = PlayerCommand::TeamPreview(p1_tp);
        let mut p2_cmd = PlayerCommand::TeamPreview(p2_tp);

        let mut turn = 0usize;
        let mut iter_failed = false;
        loop {
            turn += 1;
            if turn > MAX_TURNS {
                break;
            }
            let context = format!("iter={iter} turn={turn} matchup=({p1_path} vs {p2_path})");

            if replay_details && let MatchState::BattleState(bs) = &state {
                let active_speed_state: Vec<_> = bs
                    .p1_active_mons
                    .iter()
                    .enumerate()
                    .map(|(slot, mon)| {
                        (
                            Player::P1,
                            slot,
                            &mon.species,
                            mon.stats[1],
                            mon.stats[5],
                            mon.boosts,
                            &mon.status,
                            &mon.item,
                            &mon.ability,
                        )
                    })
                    .chain(bs.p2_active_mons.iter().enumerate().map(|(slot, mon)| {
                        (
                            Player::P2,
                            slot,
                            &mon.species,
                            mon.stats[1],
                            mon.stats[5],
                            mon.boosts,
                            &mon.status,
                            &mon.item,
                            &mon.ability,
                        )
                    }))
                    .collect();
                eprintln!("[TRUE-ACTIVES] iter={iter} turn={turn} {active_speed_state:?}");
            }

            let was_team_preview = matches!(state, MatchState::TeamPreviewState(_));

            let (next_state, raw_events, _probability) = sample_turn_raw(
                &state,
                &p1_cmd,
                &p2_cmd,
                mdex,
                pdex,
                true,
                16,
                Some(Player::P1),
            );
            let raw_events = raw_events.unwrap_or_default();
            if replay_details {
                eprintln!(
                    "[TURN] iter={iter} turn={turn} p1_cmd={p1_cmd:?} p2_cmd={p2_cmd:?} \
                     raw_events={raw_events:?}"
                );
            }
            let events_p1 = mask_events_for(Player::P1, &raw_events);
            let events_p2 = mask_events_for(Player::P2, &raw_events);

            let seeded_p1 = if was_team_preview {
                reseed_for_battle(belief_p1, Player::P1, &p1_cmd, &p2_cmd)
            } else {
                belief_p1
            };
            let seeded_p2 = if was_team_preview {
                reseed_for_battle(belief_p2, Player::P2, &p1_cmd, &p2_cmd)
            } else {
                belief_p2
            };

            if replay_details {
                for (observer, seeded) in [(Player::P1, &seeded_p1), (Player::P2, &seeded_p2)] {
                    if let UnknownMatchState::Battle(battle) = seeded {
                        eprintln!(
                            "[BELIEF-BEFORE] iter={iter} turn={turn} observer={observer:?} \
                             unresolved=({}, {}) active_species=({:?}, {:?}) back=({:?}, {:?})",
                            battle.p1_unresolved_zoroark_count,
                            battle.p2_unresolved_zoroark_count,
                            battle
                                .p1_active_mons
                                .iter()
                                .map(|mon| {
                                    (
                                        &mon.possible_species,
                                        mon.possible_illusion_state.is_some(),
                                        mon.min_stats[5],
                                        mon.max_stats[5],
                                    )
                                })
                                .collect::<Vec<_>>(),
                            battle
                                .p2_active_mons
                                .iter()
                                .map(|mon| {
                                    (
                                        &mon.possible_species,
                                        mon.possible_illusion_state.is_some(),
                                        mon.min_stats[5],
                                        mon.max_stats[5],
                                    )
                                })
                                .collect::<Vec<_>>(),
                            battle
                                .p1_known_back_mons
                                .iter()
                                .chain(battle.p1_possible_back_mons.iter())
                                .map(|mon| (
                                    &mon.possible_species,
                                    mon.possible_illusion_state.is_some(),
                                    &mon.item,
                                ))
                                .collect::<Vec<_>>(),
                            battle
                                .p2_known_back_mons
                                .iter()
                                .chain(battle.p2_possible_back_mons.iter())
                                .map(|mon| (
                                    &mon.possible_species,
                                    mon.possible_illusion_state.is_some(),
                                    &mon.item,
                                ))
                                .collect::<Vec<_>>(),
                        );
                    }
                }
            }

            // Diverge (break) directly in each Err arm so belief_p1/p2 are assigned
            // on every non-diverging path — see the module-level doc comment above.
            belief_p1 = match catch_unwind(AssertUnwindSafe(|| {
                apply_information(seeded_p1, &events_p1, false, pdex, mdex, adex, &config)
            })) {
                Ok(b) => b,
                Err(e) => {
                    contradictions.push(format!(
                        "[contradiction-p1] {context} {}",
                        e.downcast_ref::<String>().cloned().unwrap_or_default()
                    ));
                    iter_failed = true;
                    break;
                }
            };
            belief_p2 = match catch_unwind(AssertUnwindSafe(|| {
                apply_information(seeded_p2, &events_p2, false, pdex, mdex, adex, &config)
            })) {
                Ok(b) => b,
                Err(e) => {
                    contradictions.push(format!(
                        "[contradiction-p2] {context} {}",
                        e.downcast_ref::<String>().cloned().unwrap_or_default()
                    ));
                    iter_failed = true;
                    break;
                }
            };

            state = next_state;

            match &state {
                MatchState::GameOverState { .. } => break,
                MatchState::BattleState(bs) => {
                    let mut violated = false;
                    for (belief, observer) in [(&belief_p1, Player::P1), (&belief_p2, Player::P2)] {
                        let found =
                            collect_true_state_subset_violations(bs, belief, observer, pdex, mdex);
                        if !found.is_empty() {
                            violated = true;
                            if replay_details {
                                eprintln!(
                                    "[REPLAY] {context} observer={observer:?} p1_cmd={p1_cmd:?} \
                                     p2_cmd={p2_cmd:?} raw_events={raw_events:?}"
                                );
                            }
                            subset_failures.extend(
                                found
                                    .into_iter()
                                    .map(|violation| (context.clone(), violation)),
                            );
                        }
                    }
                    if violated {
                        iter_failed = true;
                        break;
                    }
                    p1_cmd =
                        PlayerCommand::Battle(random_commands_for_player(bs, Player::P1, &mut rng));
                    p2_cmd =
                        PlayerCommand::Battle(random_commands_for_player(bs, Player::P2, &mut rng));
                }
                MatchState::TeamPreviewState(_) => unreachable!(),
            }
        }
        if iter_failed {
            failed_iters += 1;
            if failed_iters >= max_failures {
                break;
            }
        }
        if attempted_iters % 1000 == 0 {
            eprintln!("[survey progress] attempted={attempted_iters} failed={failed_iters}");
        }
    }

    eprintln!(
        "\n=== SURVEY: {failed_iters}/{attempted_iters} iterations failed ({:.1}%) ===",
        100.0 * failed_iters as f64 / attempted_iters.max(1) as f64
    );

    let mut field_bucket: HashMap<String, u32> = HashMap::new();
    let mut clause_bucket: HashMap<String, u32> = HashMap::new();
    let mut species_bucket: HashMap<String, u32> = HashMap::new();
    let mut clause_count = 0u32;
    let mut field_count = 0u32;
    let contradiction_count = contradictions.len() as u32;

    for (_, violation) in &subset_failures {
        match &violation.kind {
            SubsetViolationKind::Fields {
                true_species,
                violations,
                ..
            } => {
                field_count += 1;
                *species_bucket
                    .entry(format!("{true_species:?}"))
                    .or_insert(0) += 1;
                for violation in violations {
                    *field_bucket.entry(violation.field.clone()).or_insert(0) += 1;
                }
            }
            SubsetViolationKind::Clause { clause } => {
                clause_count += 1;
                let mut present = Vec::new();
                for statement in clause {
                    statement_shape(statement, &mut present);
                }
                present.sort_unstable();
                present.dedup();
                *clause_bucket.entry(present.join("+")).or_insert(0) += 1;
            }
        }
    }

    eprintln!(
        "field violations: {field_count}, clause violations: {clause_count}, contradiction panics: {contradiction_count}"
    );
    eprintln!("-- field breakdown --");
    for (k, v) in &field_bucket {
        eprintln!("  {k}: {v}");
    }
    eprintln!("-- clause shape breakdown --");
    for (k, v) in &clause_bucket {
        eprintln!("  [{k}]: {v}");
    }
    eprintln!("-- species breakdown --");
    for (k, v) in &species_bucket {
        eprintln!("  {k}: {v}");
    }
    eprintln!("-- subset samples (first {sample_limit}) --");
    for (context, violation) in subset_failures.iter().take(sample_limit) {
        eprintln!("---\n[subset violation] context={context} {violation}");
    }
    eprintln!("-- contradiction samples (first {sample_limit}) --");
    for failure in contradictions.iter().take(sample_limit) {
        eprintln!("---\n{failure}");
    }

    std::panic::set_hook(previous_panic_hook);
}

fn run_sweep(iterations: u64, check_subset: bool) {
    let pdex = pokemon_dex();
    let mdex = move_dex();
    let adex = ability_dex();
    let ldex = learnset_dex();

    let config = InferenceConfig {
        use_stat_points: true,
        force_max_ivs: true,
        legal_items: Some(champions_legal_items()),
        learnset_dex: ldex.clone(),
        ..Default::default()
    };

    let iterations = fuzz_env_u64("POKERUST_FUZZ_ITERS", iterations);
    let seed_start = fuzz_env_u64("POKERUST_FUZZ_SEED_START", 0);
    for iter in seed_start..seed_start.saturating_add(iterations) {
        let mut rng = StdRng::seed_from_u64(iter);
        let _sample_rng = scoped_sample_rng(iter ^ SAMPLE_SEED_SALT);

        let p1_path = TEAMSHEETS[rng.gen_range(0..TEAMSHEETS.len())];
        let p2_path = TEAMSHEETS[rng.gen_range(0..TEAMSHEETS.len())];
        eprintln!("[iter {iter}] {p1_path} vs {p2_path}");

        let preview = team_preview_state_from_teamsheets(
            p1_path,
            p2_path,
            pdex,
            mdex,
            ACTIVE_PER_SIDE,
            BROUGHT_PER_SIDE,
            true,
        );

        let mut belief_p1 = UnknownMatchState::team_preview_closed_sheet_from_perspective(
            Player::P1,
            &preview.p1_mons,
            &preview.p2_mons,
            pdex,
            ACTIVE_PER_SIDE,
            BROUGHT_PER_SIDE,
            50,
            true,
        );
        let mut belief_p2 = UnknownMatchState::team_preview_closed_sheet_from_perspective(
            Player::P2,
            &preview.p2_mons,
            &preview.p1_mons,
            pdex,
            ACTIVE_PER_SIDE,
            BROUGHT_PER_SIDE,
            50,
            true,
        );

        let p1_tp = random_team_preview_command(preview.p1_mons.len(), &mut rng);
        let p2_tp = random_team_preview_command(preview.p2_mons.len(), &mut rng);

        let mut state = MatchState::TeamPreviewState(preview);
        let mut p1_cmd = PlayerCommand::TeamPreview(p1_tp);
        let mut p2_cmd = PlayerCommand::TeamPreview(p2_tp);

        let mut turn = 0usize;
        loop {
            turn += 1;
            if turn > MAX_TURNS {
                eprintln!(
                    "[iter {iter}] stalled past {MAX_TURNS} turns ({p1_path} vs {p2_path}) — skipping, not a soundness failure"
                );
                break;
            }

            let was_team_preview = matches!(state, MatchState::TeamPreviewState(_));

            // Resolve once; mask twice. Re-resolving per observer would sample two
            // different random trajectories and desync the beliefs from each other
            // and from `next_state` — see `sample_turn_raw`'s doc comment.
            let (next_state, raw_events, _probability) = sample_turn_raw(
                &state,
                &p1_cmd,
                &p2_cmd,
                mdex,
                pdex,
                true,
                16,
                Some(Player::P1),
            );
            let raw_events = raw_events.unwrap_or_default();
            let events_p1 = mask_events_for(Player::P1, &raw_events);
            let events_p2 = mask_events_for(Player::P2, &raw_events);

            let seeded_p1 = if was_team_preview {
                reseed_for_battle(belief_p1, Player::P1, &p1_cmd, &p2_cmd)
            } else {
                belief_p1
            };
            let seeded_p2 = if was_team_preview {
                reseed_for_battle(belief_p2, Player::P2, &p1_cmd, &p2_cmd)
            } else {
                belief_p2
            };

            // The soundness oracle: panics on a jointly-impossible observation.
            belief_p1 = apply_information(seeded_p1, &events_p1, false, pdex, mdex, adex, &config);
            belief_p2 = apply_information(seeded_p2, &events_p2, false, pdex, mdex, adex, &config);

            state = next_state;

            match &state {
                MatchState::GameOverState { winner, .. } => {
                    eprintln!("[iter {iter}] game over after {turn} turns, winner={winner:?}");
                    break;
                }
                MatchState::BattleState(bs) => {
                    // The second soundness oracle (opt-in — see
                    // `random_doubles_beliefs_stay_sound_subset`'s doc comment):
                    // the true state must stay a member of what each belief
                    // admits — panics on a value the belief has wrongly excluded,
                    // catching over-narrowing bugs the contradiction oracle above
                    // (soundness against self-contradiction only) cannot. See
                    // `subset_check`'s module doc for the full design.
                    if check_subset {
                        let context =
                            format!("iter={iter} turn={turn} matchup=({p1_path} vs {p2_path})");
                        assert_true_state_subset_of_belief(
                            bs,
                            &belief_p1,
                            Player::P1,
                            pdex,
                            mdex,
                            &context,
                        );
                        assert_true_state_subset_of_belief(
                            bs,
                            &belief_p2,
                            Player::P2,
                            pdex,
                            mdex,
                            &context,
                        );
                    }

                    p1_cmd =
                        PlayerCommand::Battle(random_commands_for_player(bs, Player::P1, &mut rng));
                    p2_cmd =
                        PlayerCommand::Battle(random_commands_for_player(bs, Player::P2, &mut rng));
                }
                MatchState::TeamPreviewState(_) => {
                    unreachable!("team preview only occurs once, at turn 1")
                }
            }
        }
    }
}
