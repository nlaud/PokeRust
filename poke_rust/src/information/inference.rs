//! # state::inference — information-folding engine
//!
//! Converts an ordered list of [`InformationEvent`]s into tighter bounds on the
//! [`UnknownMatchState`].  The entry point is [`apply_information`].
//!
//! ## Pipeline (six passes, one event-tree walk)
//!
//! 1. **Pass 1** — direct/structural facts (items revealed, status, types, HP, …)
//! 2. **Pass 2** — item presence/absence from behaviour (Life Orb recoil, no recoil,
//!    100%-accurate miss → Bright Powder, Choice-item multi-move → no Choice, …)
//! 3. **Pass 3** — damage → stat bounds (inverts the real pipeline via a monotone
//!    binary-search oracle; narrows BSV / EV / nature ranges from observed damage)
//! 4. **Pass 4** — speed ordering → Spe bounds (within priority brackets, with multiplier
//!    accounting; Quick Claw / Quick Draw → disjunctive predicates)
//! 5. **Pass 5** — back-solve EV / IV / nature from tightened stat bounds
//! 6. **Pass 6** — BCP (boolean constraint propagation) on the CNF `predicates` to fixpoint
//!
//! **100 % soundness guarantee**: the returned state never excludes a training that could
//! actually produce the observed events.  When events are *jointly impossible*, the function
//! **panics** via [`inference_contradiction!`] with a descriptive message.

#![allow(unused, dead_code, clippy::too_many_arguments)]

use std::collections::{HashMap, HashSet};

use crate::data::ability::Ability;
use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::information::information::{CantReason, EventKind, InformationEvent, SwitchState};
use crate::information::unknowns;
use crate::information::unknowns::{
    PokemonHP, Statement, Unknown, UnknownBattleState, UnknownMatchState, UnknownPokemonState,
    UnknownTeamPreviewState,
};
use crate::simulator::helpers::{base_damage_formula, move_has_flag, single_type_effectiveness};
use crate::state::battle::{FieldSlot, Player};
use crate::state::dex_data::{
    AbilityData, AccuracyType, MoveCategory, MoveData, MoveFlag, PokemonData, PokemonStat,
    PokemonType, PseudoWeather, SideCondition, SlotCondition, Status, Terrain, VolatileStatus,
    Weather,
};
use crate::state::pokemon::{Nature, VolatileStatusState, calc_hp, calc_stat, nature_stat_modifiers};

// ── Configuration ─────────────────────────────────────────────────────────────

/// Runtime tuning for [`apply_information`].
pub struct InferenceConfig {
    /// Use the stat-points EV lattice (`{0,4,12,20,…,252}`) instead of the full
    /// 0–252 range. Set this to match the `--stat-points` flag passed to the sim.
    pub use_stat_points: bool,
    /// Pin all opponent IVs to 31 (Pokémon Champions competitive default). When
    /// `true`, the engine skips IV uncertainty and only reasons about EVs + nature.
    pub force_max_ivs: bool,
    /// Default level for newly observed opponent Pokémon (usually 50 for Champions).
    pub level: u8,
    /// Optional legal item whitelist for the opponent. When `Some`, the inference
    /// engine restricts item disjunctions and predicates to only these items; a
    /// revealed item outside the whitelist triggers a contradiction panic. `None`
    /// means all items are considered possible.
    pub legal_items: Option<HashSet<Item>>,
    /// Whether two Pokémon on the same team may hold the same item (item clause).
    /// When `false` (the Pokémon Champions default), the engine assumes each
    /// non-`Item::None` item appears at most once per team: once a teammate's item
    /// is confirmed as `X`, `X` is excluded from every other distinct teammate's
    /// item lattice. `Item::None` (no item) is exempt and may appear on any number
    /// of teammates. When `true`, no cross-teammate exclusion is performed.
    pub allow_repeat_items: bool,
    /// Learnset data per species (from `showdownLearnsets.txt`). When non-empty,
    /// enables learnset-based Illusion narrowing: after an opponent move is revealed,
    /// any candidate species that cannot legally learn that move is dropped from
    /// `possible_species`. Empty map disables this narrowing (default for tests).
    pub learnset_dex: HashMap<Species, HashSet<PokemonMove>>,
    /// Total EV budget across all six stats. When `Some(n)`, Pass 5 applies
    /// cross-stat tightening: `max_evs[i] ≤ n − Σ_{j≠i} min_evs[j]`. Standard
    /// competitive value is 510. `None` disables the cap check.
    pub ev_total_cap: Option<u16>,
}

impl Default for InferenceConfig {
    fn default() -> Self {
        InferenceConfig {
            use_stat_points: true,
            force_max_ivs: true,
            level: 50,
            legal_items: None,
            allow_repeat_items: false,
            learnset_dex: HashMap::new(),
            ev_total_cap: Some(510),
        }
    }
}

impl InferenceConfig {
    /// Returns `true` if `item` is permitted under the configured item whitelist.
    /// When no whitelist is set every item is permitted.
    pub(crate) fn legal_item_ok(&self, item: &Item) -> bool {
        self.legal_items.as_ref().is_none_or(|l| l.contains(item))
    }
}

// ── EV lattice (stat-points mode) ─────────────────────────────────────────────

/// Achievable EV values under `--stat-points` mode.
/// Derived from `scale_evs_for_stat_points`: `ev = max(0, 8p − 4)` for `p = 0..=32`.
/// 33 values: 0, then 4, 12, 20, …, 252 (each +8 after the first gap).
pub const EV_LATTICE: [u8; 33] = [
    0, 4, 12, 20, 28, 36, 44, 52, 60, 68, 76, 84, 92, 100, 108, 116, 124, 132, 140, 148, 156, 164,
    172, 180, 188, 196, 204, 212, 220, 228, 236, 244, 252,
];

// ── Contradiction macro ────────────────────────────────────────────────────────

thread_local! {
    /// A human-readable breadcrumb of "what the engine was doing" when an
    /// `inference_contradiction!` panic fires. Set to a whole-turn event summary at
    /// the top of `apply_information_battle` (covers the tail BCP/Pass-4 passes, which
    /// run *after* the full event walk and so aren't tied to any single event), then
    /// refined to the specific node's `EventKind` by `process_battle_event` as the
    /// depth-first Pass 1–3 walk descends.
    ///
    /// Deliberately a `thread_local!` rather than a parameter threaded through every
    /// `inference_contradiction!` call site (~30 of them, several deep in `bcp.rs` with
    /// no natural place to plumb new state) — `cargo test` runs tests on separate OS
    /// threads, so this stays test-isolated without any synchronization. Referencing it
    /// by plain name from inside the macro body below resolves at the macro's
    /// *definition* site (this module) regardless of which module invokes the macro,
    /// so `bcp.rs`'s call sites pick it up with no extra plumbing.
    static CURRENT_EVENT_CONTEXT: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Panic with a descriptive contradiction message.  Called whenever the observed
/// events are jointly impossible under the current state.
macro_rules! inference_contradiction {
    ($ctx:expr, $($msg:tt)*) => {{
        let __event_ctx = $crate::information::inference::CURRENT_EVENT_CONTEXT
            .with(|c| c.borrow().clone());
        panic!(
            "[inference contradiction] context={:?} event={} — {}",
            $ctx,
            __event_ctx.as_deref().unwrap_or("<none>"),
            format!($($msg)*)
        )
    }};
}

mod bcp;

// ── Zoroark / Illusion parallel-hypothesis mirroring ──────────────────────────
//
// A mon that could secretly be a disguised Illusion user (Zoroark line) carries
// a full second `UnknownPokemonState` in `possible_illusion_state` — see that
// field's doc comment in `unknowns.rs` for the design. Every per-mon narrowing
// operation that can panic via `inference_contradiction!` (reveal handling,
// stat-bound tightening, etc.) is applied through `apply_with_illusion_mirroring`
// instead of being called directly on the primary mon, so the SAME evidence is
// replayed against the hypothesis:
//   - hypothesis rejects the operation           → not Zoroark; drop it.
//   - primary rejects, hypothesis accepts         → IS Zoroark; promote.
//   - both accept                                 → keep both, unchanged.
//   - both reject                                 → genuine contradiction; panics
//                                                    exactly as it would without
//                                                    any hypothesis in play.
// This needs no changes to any existing function's signature or panic behavior —
// every extracted `f` is reused verbatim for both the primary and the mirrored
// call, so there is no way for the two hypotheses' logic to drift apart.

/// Outcome of [`apply_with_illusion_mirroring`], telling the caller whether it
/// needs to reconcile side-wide Zoroark bookkeeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IllusionMirrorOutcome {
    /// No live hypothesis existed, or both the primary and the hypothesis
    /// accepted the operation. Nothing further to do.
    Unchanged,
    /// A live hypothesis rejected the operation and was dropped
    /// (`mon.possible_illusion_state` is now `None`). The primary was mutated
    /// normally (it necessarily accepted, or this call would have panicked).
    HypothesisRejected,
    /// The primary rejected the operation but the hypothesis accepted it —
    /// this mon has been resolved as Zoroark. `mon`'s fields now hold the
    /// resolved identity and `possible_illusion_state` is `None`. The caller
    /// **must** follow up with `resolve_zoroark_globally` for this mon's side.
    Promoted,
}

/// Apply a fallible per-mon narrowing operation `f` to `mon`, mirroring it onto
/// `mon.possible_illusion_state` (if live) per the four-way outcome above.
///
/// **Contract on `f`**: it must be the exact same operation already used for the
/// primary elsewhere in the pipeline (never a re-derived/parallel implementation
/// — that would let the two hypotheses drift apart), and it must only read
/// state outside `mon` (dex/move data, other mons via an already-computed
/// snapshot, etc.) — never mutate anything outside `mon` itself. Passes whose
/// per-mon step also needs to emit shared state (e.g. CNF clauses referencing
/// this mon's `mon_idx`) need a different wiring; this helper only covers the
/// self-contained case (Pass 1 reveals, Pass 3/4/5 per-mon bound tightening).
///
/// Soundness note: a panic caught here mid-mutation of the sub-state (or, in
/// the promotion-check branch, mid-mutation of the primary) is never observed
/// in a half-narrowed condition — `sub` is a private owned value only written
/// back to `mon.possible_illusion_state` on the success path, and the primary
/// promotion-check reborrow only replaces `mon`'s content wholesale on success
/// (via `promote_illusion_to_primary`) rather than leaving partial mutations.
pub(super) fn apply_with_illusion_mirroring<F>(
    mon: &mut UnknownPokemonState,
    f: F,
) -> IllusionMirrorOutcome
where
    F: Fn(&mut UnknownPokemonState),
{
    let Some(boxed_sub) = mon.possible_illusion_state.take() else {
        f(mon);
        return IllusionMirrorOutcome::Unchanged;
    };

    let mut sub = boxed_sub;
    let sub_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut sub)));

    match sub_result {
        Err(_) => {
            // Hypothesis infeasible under this evidence: this mon is not
            // Zoroark. Drop the hypothesis; run the primary for real — if
            // IT ALSO panics, that's a genuine, unresolvable contradiction
            // and must propagate exactly as it would with no hypothesis.
            f(mon);
            IllusionMirrorOutcome::HypothesisRejected
        }
        Ok(()) => {
            // Hypothesis still feasible. Try the primary too, but catch its
            // panic here instead of letting it propagate: if the PRIMARY is
            // the one that's infeasible, that IS "this mon is Zoroark" —
            // not a genuine contradiction.
            let primary_result = {
                let reborrow: &mut UnknownPokemonState = mon;
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || f(reborrow)))
            };
            match primary_result {
                Ok(()) => {
                    mon.possible_illusion_state = Some(sub);
                    IllusionMirrorOutcome::Unchanged
                }
                Err(_) => {
                    promote_illusion_to_primary(mon, *sub);
                    IllusionMirrorOutcome::Promoted
                }
            }
        }
    }
}

/// Mirror a non-fallible per-mon mutation (one that can never panic / contradict
/// — a pure state transition like clearing per-turn flags on switch-out) onto
/// `mon`'s live Zoroark sub-state, if any. Unlike `apply_with_illusion_mirroring`,
/// no `catch_unwind` is needed since `f` can't fail; this just keeps the
/// sub-state's own physically-observable fields in lockstep with the primary's,
/// since both are meant to describe the same physical mon at the same moment.
pub(super) fn mirror_infallible_on_illusion<F>(mon: &mut UnknownPokemonState, f: F)
where
    F: FnOnce(&mut UnknownPokemonState),
{
    if let Some(sub) = mon.possible_illusion_state.as_deref_mut() {
        f(sub);
    }
}

/// Called when a mon's own hypothesis (`possible_illusion_state`) has just been
/// proven correct — the primary (shown-species) identity is infeasible, but
/// "this physical mon is Zoroark" remains consistent with every observation so
/// far. Replaces the mon's entire content with the resolved hypothesis (species,
/// types, ability, moves, item, nature, stat/EV/IV bounds — every field the two
/// hypotheses could have differed on). Physically-observable fields (HP, status,
/// boosts, volatiles, `times_hit`, …) are identical between the two hypotheses
/// by construction (both track the same real physical mon under the same event
/// stream), so a wholesale replace is safe.
///
/// Does NOT touch side-wide bookkeeping (the `possible_back` Zoroark baseline
/// entry, sibling mons' own `possible_illusion_state`, the unresolved-Zoroark
/// count) — callers **must** follow up with `resolve_zoroark_globally`.
pub(super) fn promote_illusion_to_primary(
    mon: &mut UnknownPokemonState,
    resolved: UnknownPokemonState,
) {
    *mon = resolved;
    // Defensive; should already be `None` per the "no nesting" invariant.
    mon.possible_illusion_state = None;
}

/// Every `UnknownPokemonState` belonging to `side` — active, known-back,
/// possible-back, AND fainted — mutably, for side-wide Zoroark bookkeeping.
/// (Unlike `combined_back`, which deliberately excludes fainted mons from
/// "could this be a hidden bench mon" reasoning, this needs to reach a
/// fainted mon's `possible_illusion_state` too: a hypothesis attached before
/// it fainted must still be dropped once the side's Zoroark is resolved.)
pub(super) fn side_mons_mut(
    state: &mut UnknownBattleState,
    side: Player,
) -> impl Iterator<Item = &mut UnknownPokemonState> {
    match side {
        Player::P1 => state
            .p1_active_mons
            .iter_mut()
            .chain(state.p1_known_back_mons.iter_mut())
            .chain(state.p1_possible_back_mons.iter_mut())
            .chain(state.p1_fainted_mons.iter_mut()),
        Player::P2 => state
            .p2_active_mons
            .iter_mut()
            .chain(state.p2_known_back_mons.iter_mut())
            .chain(state.p2_possible_back_mons.iter_mut())
            .chain(state.p2_fainted_mons.iter_mut()),
    }
}

/// Called once a Pokémon's true identity has been positively pinned down as
/// (or ruled out as) this side's Illusion forme — via `promote_illusion_to_primary`,
/// an `IllusionEnded` reveal, the Illusion forme itself entering undisguised, or
/// the doubles two-active-of-the-same-species case. Decrements
/// `p{side}_unresolved_zoroark_count`; once it reaches 0 every remaining
/// `possible_illusion_state` hypothesis on the side is now moot (Zoroark's
/// location(s) are fully accounted for) and is dropped.
///
/// Does NOT remove the resolved mon's own bench entry from `possible_back`/
/// `known_back` if it still has one elsewhere — callers that specifically
/// resolved a mon FROM the bench (rather than from an active slot) are
/// responsible for that removal themselves, since only they know which entry
/// was consumed.
pub(super) fn resolve_zoroark_globally(state: &mut UnknownBattleState, side: Player) {
    let count = match side {
        Player::P1 => &mut state.p1_unresolved_zoroark_count,
        Player::P2 => &mut state.p2_unresolved_zoroark_count,
    };
    *count = count.saturating_sub(1);
    if *count == 0 {
        for mon in side_mons_mut(state, side) {
            mon.possible_illusion_state = None;
        }
    }
}

/// Shared follow-up for EVERY `IllusionMirrorOutcome::Promoted` call site — Pass 1
/// move/item reveals, and Pass 3's stat-tightening backstop surfacing through
/// Pass 5 — not just the dedicated `IllusionEnded` event handler. `discarded_species`
/// is the mon's `possible_species` captured by the caller BEFORE
/// `apply_with_illusion_mirroring` overwrote it with the resolved (Zoroark)
/// identity: that shown species was never really on the field and must be
/// restored to `possible_back`, exactly like `IllusionEnded`'s handler does for a
/// direct-damage/ability-change reveal.
///
/// Without this, a promotion triggered by move-legality (or a damage/stat
/// contradiction) — which can and does fire BEFORE the disguise visibly breaks
/// in-game — silently drops the decoy species from the tracked roster forever:
/// by the time (if ever) the real `IllusionEnded` event later arrives, this
/// mon's `possible_species` already reads as the resolved Zoroark identity, so
/// that handler's own "what was discarded" capture no longer sees the decoy
/// either, and its `is_illusion_capable_species` guard skips the restore
/// entirely (it looks like there's nothing left to discard). Calling this at
/// EVERY promotion site closes that gap at the moment it actually happens.
///
/// Guarded exactly like `IllusionEnded`'s own restore: a no-op if
/// `discarded_species` wasn't `Known` (defensive; `possible_species` is always
/// `Known` pre-promotion under the current model), if it's itself
/// Illusion-capable (promotion only ever fires FROM a non-Illusion-capable shown
/// identity), or if a matching bench entry already exists (e.g. `IllusionEnded`
/// got here first this same turn).
fn finish_illusion_promotion_restore(
    state: &mut UnknownBattleState,
    side: Player,
    discarded_species: Unknown<Species>,
    dex: &HashMap<Species, PokemonData>,
    config: &InferenceConfig,
) {
    let Unknown::Known(discarded) = discarded_species else { return };
    if unknowns::is_illusion_capable_species(&discarded) {
        return;
    }
    if combined_back(state, &side).iter().any(|m| unknown_is_known_as(&m.possible_species, &discarded)) {
        return;
    }
    restore_discarded_primary_to_bench(state, side, discarded, dex, config);
}

/// Look up `species`'s pristine team-preview snapshot in `side`'s
/// `p{side}_roster_templates` (see that field's doc comment on
/// `UnknownBattleState`). `None` under species-only (non-open-sheet) preview —
/// the templates list is empty there — or for a species genuinely outside the
/// team-preview roster (a synthetic test scenario). Callers must fall back to
/// `from_opponent_species` in that case, exactly as they did before templates
/// existed.
fn find_roster_template<'a>(
    state: &'a UnknownBattleState,
    side: Player,
    species: &Species,
) -> Option<&'a UnknownPokemonState> {
    let templates = match side {
        Player::P1 => &state.p1_roster_templates,
        Player::P2 => &state.p2_roster_templates,
    };
    templates.iter().find(|m| unknown_is_known_as(&m.possible_species, species))
}

/// Companion to a promotion (e.g. via `IllusionEnded`): the mon's PRIMARY identity
/// being discarded (e.g. "Snorlax") was never actually confirmed to be on the
/// field at all — the active slot was secretly this side's Illusion forme in
/// disguise the whole time. That means the real `discarded_species` must still be
/// unaccounted for somewhere in the party. Restore it to `possible_back`.
///
/// Prefers cloning `discarded_species`'s pristine team-preview snapshot
/// (`find_roster_template`) over rebuilding species-only: under an open team
/// sheet, that snapshot already carries the fully-`Known` item/moves/ability/
/// nature set team preview revealed, and everything "observed" about the
/// discarded identity while it looked active was actually the Illusion forme's
/// behavior misattributed to it — none of it is trustworthy information about
/// the real mon, but the PRE-BATTLE sheet reveal still is. Falls back to the old
/// species-only baseline only when no template exists (species-only preview, or
/// a species outside the known roster). Sound either way, if imprecise in the
/// fallback case (the cost of having briefly guessed wrong about which physical
/// mon was on the field — never a soundness gap).
///
/// Callers must first confirm no matching bench entry already exists for
/// `discarded_species` (see the `IllusionEnded` handler) — this function
/// unconditionally pushes a new entry and would otherwise create a phantom
/// duplicate roster member.
pub(super) fn restore_discarded_primary_to_bench(
    state: &mut UnknownBattleState,
    side: Player,
    discarded_species: Species,
    dex: &HashMap<Species, PokemonData>,
    config: &InferenceConfig,
) {
    let mut restored = if let Some(template) = find_roster_template(state, side, &discarded_species) {
        let mut t = template.clone();
        t.possible_illusion_state = None; // defensive; templates never carry one
        t
    } else {
        let mut restored =
            UnknownPokemonState::from_opponent_species(discarded_species.clone(), dex, config.level);
        recompute_stats_for_iv_mode(&mut restored, &discarded_species, dex, config);
        restored
    };
    // F2: if the side STILL has an unresolved Illusion forme after this
    // restoration (a non-Species-Clause format with more than one Illusion-capable
    // roster member — see `p{side}_unresolved_zoroark_count`'s doc comment), this
    // freshly-rebuilt entry is itself eligible to be that OTHER Zoroark and must
    // not be silently treated as ruled out. Under the ordinary single-Zoroark case
    // this is a no-op: `resolve_zoroark_globally` (called by every caller of this
    // function before it) already drops the count to 0 and clears every hypothesis
    // side-wide once the side's one Illusion forme is positively located.
    maybe_seed_fresh_hypothesis(state, side, &mut restored);
    match side {
        Player::P1 => state.p1_possible_back_mons.push(restored),
        Player::P2 => state.p2_possible_back_mons.push(restored),
    }
}

/// Find `side`'s real Illusion-forme roster member — the template to seed a fresh
/// hypothesis from — checking the live bench first (`combined_back`, in case it's
/// still sitting there unresolved) and falling back to the pristine team-preview
/// snapshot (`p{side}_roster_templates`) for the case where its live bench entry
/// was already consumed or removed by earlier bookkeeping.
fn find_illusion_baseline(state: &UnknownBattleState, side: Player) -> Option<UnknownPokemonState> {
    let is_baseline = |m: &&UnknownPokemonState| {
        matches!(&m.possible_species, Unknown::Known(s) if unknowns::is_illusion_capable_species(s))
    };
    combined_back(state, &side).into_iter().find(is_baseline).cloned().or_else(|| {
        let templates = match side {
            Player::P1 => &state.p1_roster_templates,
            Player::P2 => &state.p2_roster_templates,
        };
        templates.iter().find(|m| is_baseline(m)).cloned()
    })
}

/// If `side` still has an unresolved Illusion forme, and `mon` is neither itself
/// Illusion-capable nor already carrying a hypothesis, seed one from the side's
/// Illusion-forme baseline. A no-op whenever the side's count is already 0 — under
/// the ordinary Species-Clause case (at most one Illusion-capable roster member),
/// that's always true by the time a rebuild path like `restore_discarded_primary_to_bench`
/// runs, since the side's one Illusion forme was just positively located. This only
/// does real work for a hypothetical multi-Illusion-forme roster, where a second,
/// still-unresolved Illusion forme could legitimately be this freshly-rebuilt mon.
fn maybe_seed_fresh_hypothesis(state: &UnknownBattleState, side: Player, mon: &mut UnknownPokemonState) {
    let unresolved = match side {
        Player::P1 => state.p1_unresolved_zoroark_count,
        Player::P2 => state.p2_unresolved_zoroark_count,
    };
    if unresolved == 0 || mon.possible_illusion_state.is_some() {
        return;
    }
    if matches!(&mon.possible_species, Unknown::Known(s) if unknowns::is_illusion_capable_species(s)) {
        return;
    }
    if let Some(baseline) = find_illusion_baseline(state, side) {
        mon.possible_illusion_state = Some(Box::new(unknowns::seed_illusion_hypothesis_for(mon, &baseline)));
    }
}

// ── mon_idx helpers ────────────────────────────────────────────────────────────
//
// S1: `mon_idx` order is `[p1_active…, p2_active…, p1_known_back…, p1_possible_back…,
// p2_known_back…, p2_possible_back…]` — both active segments come first, before
// either side's bench. The naive per-side-contiguous layout (`[p1_active, p1_back,
// p2_active, p2_back]`) had a staleness bug: bench Vecs grow/shrink on switch (`push`
// out, `Vec::remove` in), shifting every later index. With P1's bench sitting between
// P1's actives and P2's actives, any P1 switch silently shifted P2's active mon_idx —
// and persisted `Statement`s (SpeedComparison, weather/terrain/screen setters,
// HasItem/HasAbility clauses) would then retarget the wrong physical Pokémon.
//
// With both actives fixed at the front, `p1_active_mons.len()` / `p2_active_mons.len()`
// are the only inputs to computing an active mon's index, and both stay stable for
// the rest of the battle (`pass1_switch` overwrites an active slot in place, never
// push/removes). Bench indices remain unstable, but nothing persists a bench index
// across events (`teammate_indices`/`enforce_unique_item` compute and consume them
// atomically within one event), so this is sound.
//
// Trade-off: a side's full roster is no longer one contiguous range (P1's bench sits
// after P2's active). Helpers needing "all mons on side X" (`teammate_indices`,
// `TeamStatusCured`, `mon_is_p2`) must check each segment explicitly.

/// The six roster-segment sizes needed to resolve any `mon_idx`, in `mon_idx` order.
struct MonSegments {
    p1_active: usize,
    p2_active: usize,
    p1_known_back: usize,
    p1_possible_back: usize,
    p2_known_back: usize,
    p2_possible_back: usize,
}

impl MonSegments {
    fn of(state: &UnknownBattleState) -> Self {
        MonSegments {
            p1_active: state.p1_active_mons.len(),
            p2_active: state.p2_active_mons.len(),
            p1_known_back: state.p1_known_back_mons.len(),
            p1_possible_back: state.p1_possible_back_mons.len(),
            p2_known_back: state.p2_known_back_mons.len(),
            p2_possible_back: state.p2_possible_back_mons.len(),
        }
    }
    /// `[p1_active_range, p2_active_range, p1_back_range, p2_back_range]`, each a
    /// contiguous `mon_idx` range. `p1_back` combines known+possible (always
    /// adjacent), likewise `p2_back`.
    fn ranges(&self) -> [std::ops::Range<usize>; 4] {
        let p1_active = 0..self.p1_active;
        let p2_active = self.p1_active..(self.p1_active + self.p2_active);
        let back_start = self.p1_active + self.p2_active;
        let p1_back_end = back_start + self.p1_known_back + self.p1_possible_back;
        let p1_back = back_start..p1_back_end;
        let p2_back = p1_back_end..(p1_back_end + self.p2_known_back + self.p2_possible_back);
        [p1_active, p2_active, p1_back, p2_back]
    }
}

/// Total number of mons tracked in a `BattleState`, in `mon_idx` order.
fn mons_count_battle(state: &UnknownBattleState) -> usize {
    state.p1_active_mons.len()
        + state.p2_active_mons.len()
        + state.p1_known_back_mons.len()
        + state.p1_possible_back_mons.len()
        + state.p2_known_back_mons.len()
        + state.p2_possible_back_mons.len()
}

/// Return the `mon_idx` of the Pokémon currently occupying `slot` in the active array.
pub fn mon_idx_for_active_slot(state: &UnknownBattleState, slot: &FieldSlot) -> Option<usize> {
    let slot_i = slot.slot_index as usize;
    match slot.player {
        Player::P1 => {
            if slot_i < state.p1_active_mons.len() {
                Some(slot_i)
            } else {
                None
            }
        }
        Player::P2 => {
            if slot_i < state.p2_active_mons.len() {
                Some(p2_mon_start(state) + slot_i)
            } else {
                None
            }
        }
    }
}

/// Borrow the `UnknownPokemonState` at `mon_idx`.
pub fn get_mon_by_idx(state: &UnknownBattleState, idx: usize) -> Option<&UnknownPokemonState> {
    let segs: [&[UnknownPokemonState]; 6] = [
        &state.p1_active_mons,
        &state.p2_active_mons,
        &state.p1_known_back_mons,
        &state.p1_possible_back_mons,
        &state.p2_known_back_mons,
        &state.p2_possible_back_mons,
    ];
    let mut offset = 0;
    for seg in segs {
        if idx < offset + seg.len() {
            return Some(&seg[idx - offset]);
        }
        offset += seg.len();
    }
    None
}

/// Mutably borrow the `UnknownPokemonState` at `mon_idx`.
pub fn get_mon_mut_by_idx(
    state: &mut UnknownBattleState,
    idx: usize,
) -> Option<&mut UnknownPokemonState> {
    let p1a = state.p1_active_mons.len();
    let p2a = state.p2_active_mons.len();
    let p1k = state.p1_known_back_mons.len();
    let p1p = state.p1_possible_back_mons.len();
    let p2k = state.p2_known_back_mons.len();

    if idx < p1a {
        return Some(&mut state.p1_active_mons[idx]);
    }
    let idx = idx - p1a;
    if idx < p2a {
        return Some(&mut state.p2_active_mons[idx]);
    }
    let idx = idx - p2a;
    if idx < p1k {
        return Some(&mut state.p1_known_back_mons[idx]);
    }
    let idx = idx - p1k;
    if idx < p1p {
        return Some(&mut state.p1_possible_back_mons[idx]);
    }
    let idx = idx - p1p;
    if idx < p2k {
        return Some(&mut state.p2_known_back_mons[idx]);
    }
    let idx = idx - p2k;
    if idx < state.p2_possible_back_mons.len() {
        return Some(&mut state.p2_possible_back_mons[idx]);
    }
    None
}

/// Render every `mon_idx` in this state alongside its segment tag and current
/// `possible_species`, e.g. `0:p1_active=Known(Tyranitar) 1:p1_active=Known(Lycanroc)
/// 2:p2_active=Known(Charizard) …`. Purely a debugging aid for
/// `inference_contradiction!` call sites: a clause that names a `mon_idx` whose
/// legend entry is an unexpected species (or the wrong side) is the signature of
/// an S1 index-shift bug — see the `mon_idx` header comment above. Not used for
/// any inference logic itself.
pub(super) fn mon_idx_legend(state: &UnknownBattleState) -> String {
    let segs: [(&str, &[UnknownPokemonState]); 6] = [
        ("p1_active", &state.p1_active_mons),
        ("p2_active", &state.p2_active_mons),
        ("p1_known_back", &state.p1_known_back_mons),
        ("p1_possible_back", &state.p1_possible_back_mons),
        ("p2_known_back", &state.p2_known_back_mons),
        ("p2_possible_back", &state.p2_possible_back_mons),
    ];
    let mut out = String::new();
    let mut idx = 0;
    for (tag, seg) in segs {
        for mon in seg {
            if idx > 0 {
                out.push(' ');
            }
            out.push_str(&format!("{idx}:{tag}={:?}", mon.possible_species));
            idx += 1;
        }
    }
    out
}

// ── Unknown<T> manipulation helpers ───────────────────────────────────────────

/// Add `val` to the exclusion list.  Contradiction if already `Known` to `val`.
/// Removes `val` from a `Possibly` set; collapses to `Known` if one remains.
fn unknown_exclude<T: PartialEq + Clone + std::fmt::Debug>(u: &mut Unknown<T>, val: &T, ctx: &str) {
    match u {
        Unknown::Known(v) => {
            if v == val {
                inference_contradiction!(ctx, "exclude({:?}) conflicts with Known value", val);
            }
        }
        Unknown::Not(excluded) => {
            if !excluded.contains(val) {
                excluded.push(val.clone());
            }
        }
        Unknown::Possibly(candidates) => {
            candidates.retain(|c| c != val);
            if candidates.len() == 1 {
                *u = Unknown::Known(candidates[0].clone());
            }
        }
    }
}

/// Force an `Unknown<T>` to `Known(val)`.  Contradiction if already `Known` to
/// something else, or if `val` is in a `Not` exclusion list.
fn unknown_set_known<T: PartialEq + Clone + std::fmt::Debug>(
    u: &mut Unknown<T>,
    val: T,
    ctx: &str,
) {
    match u {
        Unknown::Known(v) => {
            if *v != val {
                inference_contradiction!(ctx, "Known({:?}) vs new Known({:?})", v, val);
            }
        }
        Unknown::Not(excluded) => {
            if excluded.contains(&val) {
                inference_contradiction!(
                    ctx,
                    "Not({:?}) excludes the revealed value {:?}",
                    excluded,
                    val
                );
            }
            *u = Unknown::Known(val);
        }
        Unknown::Possibly(candidates) => {
            if !candidates.contains(&val) {
                inference_contradiction!(
                    ctx,
                    "Possibly({:?}) does not include {:?}",
                    candidates,
                    val
                );
            }
            *u = Unknown::Known(val);
        }
    }
}

/// `true` if `val` is definitely excluded (not possible).
pub fn unknown_is_excluded<T: PartialEq>(u: &Unknown<T>, val: &T) -> bool {
    match u {
        Unknown::Known(v) => v != val,
        Unknown::Not(excluded) => excluded.contains(val),
        Unknown::Possibly(candidates) => !candidates.iter().any(|c| c == val),
    }
}

/// `true` if this `Unknown` is `Known` to exactly `val`.
fn unknown_is_known_as<T: PartialEq>(u: &Unknown<T>, val: &T) -> bool {
    matches!(u, Unknown::Known(v) if v == val)
}

/// Widen to whichever of `a`/`b` admits more: `x` is possible in the result iff it
/// was possible under `a` OR under `b`. Used when the same slot might really be one
/// of two distinct physical identities (Illusion) and each identity's own bound needs
/// folding into one marginal — never narrows past what either side alone allowed, so
/// this can only ever widen, not exclude. `pub(crate)` so `describe.rs` can union a
/// primary field with its live Zoroark hypothesis for display ("A or B" rendering).
pub(crate) fn unknown_union<T: PartialEq + Clone>(a: &Unknown<T>, b: &Unknown<T>) -> Unknown<T> {
    match (a, b) {
        (Unknown::Known(x), Unknown::Known(y)) => {
            if x == y { Unknown::Known(x.clone()) } else { Unknown::Possibly(vec![x.clone(), y.clone()]) }
        }
        (Unknown::Known(x), Unknown::Possibly(ys)) | (Unknown::Possibly(ys), Unknown::Known(x)) => {
            let mut out = ys.clone();
            if !out.contains(x) {
                out.push(x.clone());
            }
            Unknown::Possibly(out)
        }
        (Unknown::Possibly(xs), Unknown::Possibly(ys)) => {
            let mut out = xs.clone();
            for y in ys {
                if !out.contains(y) {
                    out.push(y.clone());
                }
            }
            Unknown::Possibly(out)
        }
        (Unknown::Known(x), Unknown::Not(excluded)) | (Unknown::Not(excluded), Unknown::Known(x)) => {
            // "Everything except `excluded`" unioned with a single extra value: drop
            // that value from the exclusion list if it was there, otherwise unchanged
            // (it was already included).
            let mut out = excluded.clone();
            out.retain(|e| e != x);
            Unknown::Not(out)
        }
        (Unknown::Possibly(xs), Unknown::Not(excluded)) | (Unknown::Not(excluded), Unknown::Possibly(xs)) => {
            let mut out = excluded.clone();
            out.retain(|e| !xs.contains(e));
            Unknown::Not(out)
        }
        (Unknown::Not(ex_a), Unknown::Not(ex_b)) => {
            // "Almost everything" unioned with "almost everything": an item is only
            // excluded from the union if BOTH sides excluded it — i.e. the intersection
            // of the two exclusion lists.
            let out: Vec<T> = ex_a.iter().filter(|e| ex_b.contains(e)).cloned().collect();
            Unknown::Not(out)
        }
    }
}

/// Candidate values currently admitted by `u`, if that set is small enough to be
/// worth encoding as an explicit CNF disjunction (`Possibly`/`Known`). `None` for
/// `Not(excluded)` — an "almost everything" bound would blow up into a near-tautological
/// clause covering hundreds of items, so callers should skip clause emission for that
/// side rather than materialize it.
fn unknown_bounded_candidates<T: Clone>(u: &Unknown<T>) -> Option<Vec<T>> {
    match u {
        Unknown::Known(v) => Some(vec![v.clone()]),
        Unknown::Possibly(vs) => Some(vs.clone()),
        Unknown::Not(_) => None,
    }
}

// ── Item-clause helpers ────────────────────────────────────────────────────────

/// Return the `mon_idx` values for every Pokémon on the same side as `source_idx`,
/// excluding `source_idx` itself. Used by item-clause propagation.
///
/// Assumes every entry in the six roster lists is a pairwise-distinct physical mon.
/// Holds today (`possible_back` is unpopulated), but would need gating if future
/// Illusion modeling ever let two `possible_back` entries alias the same slot.
// TODO: revisit if possible_back ever holds non-distinct Illusion hypotheses.
///
/// S1: walks the P1/P2 range pair from `MonSegments::ranges()` rather than a single
/// `[start, end)` span, since a side's roster isn't contiguous (see mon_idx header).
fn teammate_indices(state: &UnknownBattleState, source_idx: usize) -> Vec<usize> {
    let [p1_active, p2_active, p1_back, p2_back] = MonSegments::of(state).ranges();

    let is_p1 = p1_active.contains(&source_idx) || p1_back.contains(&source_idx);
    let is_p2 = p2_active.contains(&source_idx) || p2_back.contains(&source_idx);

    let side_ranges = if is_p1 {
        [p1_active, p1_back]
    } else if is_p2 {
        [p2_active, p2_back]
    } else {
        return vec![];
    };

    side_ranges
        .into_iter()
        .flatten()
        .filter(|&i| i != source_idx)
        .collect()
}

/// Under item clause, exclude `item` from every distinct teammate of the mon at
/// `source_idx`. No-op when `allow_repeat_items` is `true` or `item` is
/// `Item::None` (no-item may appear on multiple mons freely).
fn enforce_unique_item(
    state: &mut UnknownBattleState,
    source_idx: usize,
    item: &Item,
    allow_repeat_items: bool,
) {
    if allow_repeat_items || *item == Item::None {
        return;
    }
    for idx in teammate_indices(state, source_idx) {
        if let Some(mon) = get_mon_mut_by_idx(state, idx) {
            unknown_exclude(&mut mon.item, item, &format!("item-clause#{idx}"));
        }
    }
}

// ── Public entry point ─────────────────────────────────────────────────────────

/// Fold one turn's (or team preview's) ordered `events` into `state`, returning
/// an updated `UnknownMatchState` that incorporates every fact the events imply.
///
/// `ability_dex` supplies ability metadata (on-start visibility, priority modifiers).
/// Pass `&HashMap::new()` if not available — ability-absence inference is silently skipped.
///
/// # Panics
/// If the events are jointly impossible under the current state (soundness oracle).
pub fn apply_information(
    mut state: UnknownMatchState,
    events: &[InformationEvent],
    is_team_preview: bool,
    dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    ability_dex: &HashMap<Ability, AbilityData>,
    config: &InferenceConfig,
) -> UnknownMatchState {
    match &mut state {
        UnknownMatchState::TeamPreview(preview) => {
            apply_information_team_preview(preview, events, config);
        }
        UnknownMatchState::Battle(battle) => {
            apply_information_battle(battle, events, dex, move_dex, ability_dex, config);
        }
        UnknownMatchState::GameOver { .. } => {}
    }
    state
}

// ── Team-preview path ─────────────────────────────────────────────────────────

fn apply_information_team_preview(
    state: &mut UnknownTeamPreviewState,
    events: &[InformationEvent],
    config: &InferenceConfig,
) {
    // slot_map: (player, field_slot_index) → index in p1_mons / p2_mons.
    // Persists across top-level events so reactions at any nesting depth can
    // look up the right mon after a SimultaneousSwitch.
    let mut slot_map: Vec<(Player, u8, usize)> = Vec::new();
    for event in events {
        process_team_preview_event(state, event, config, &mut slot_map);
    }
}

/// Look up the `UnknownPokemonState` currently occupying `(player, slot_index)`.
/// Returns `None` if the slot has not been filled by a switch yet.
fn find_preview_mon<'a>(
    state: &'a mut UnknownTeamPreviewState,
    player: &Player,
    slot_index: u8,
    slot_map: &[(Player, u8, usize)],
) -> Option<&'a mut UnknownPokemonState> {
    let mon_idx = slot_map.iter().find_map(|(p, s, idx)| {
        if p == player && *s == slot_index { Some(*idx) } else { None }
    })?;
    match player {
        Player::P1 => state.p1_mons.get_mut(mon_idx),
        Player::P2 => state.p2_mons.get_mut(mon_idx),
    }
}

fn process_team_preview_event(
    state: &mut UnknownTeamPreviewState,
    event: &InformationEvent,
    config: &InferenceConfig,
    // (player, field_slot_index) → roster index in p1_mons / p2_mons.
    slot_map: &mut Vec<(Player, u8, usize)>,
) {
    // Shared body for Switch / SimultaneousSwitch — register the field slot in the
    // slot map and apply the visible switch state to the matching roster mon.
    fn register_preview_switch(
        state: &mut UnknownTeamPreviewState,
        sw: &SwitchState,
        config: &InferenceConfig,
        slot_map: &mut Vec<(Player, u8, usize)>,
    ) {
        let mons = match sw.slot.player {
            Player::P1 => &mut state.p1_mons,
            Player::P2 => &mut state.p2_mons,
        };
        if let Some(roster_idx) = mons
            .iter()
            .position(|m| unknown_is_known_as(&m.possible_species, &sw.species))
        {
            apply_switch_state_to_mon(&mut mons[roster_idx], sw, config);
            slot_map.push((sw.slot.player, sw.slot.slot_index, roster_idx));
        }
    }

    match &event.kind {
        // ── Switch-in events — register the field slot in the slot map ────────
        EventKind::Switch(sw) => register_preview_switch(state, sw, config, slot_map),

        EventKind::SimultaneousSwitch { switches } => {
            for sw in switches {
                register_preview_switch(state, sw, config, slot_map);
            }
        }

        // ── HP changes ───────────────────────────────────────────────────────
        EventKind::DamageDealt { target, new_hp, .. } => {
            if let Some(mon) =
                find_preview_mon(state, &target.player, target.slot_index, slot_map)
            {
                mon.hp = new_hp.clone();
            }
        }

        EventKind::Healed { target, new_hp, .. } => {
            if let Some(mon) =
                find_preview_mon(state, &target.player, target.slot_index, slot_map)
            {
                mon.hp = new_hp.clone();
            }
        }

        // ── Fainting from entry damage ────────────────────────────────────────
        EventKind::Faint { slot } => {
            if let Some(mon) =
                find_preview_mon(state, &slot.player, slot.slot_index, slot_map)
            {
                mon.fainted = true;
            }
        }

        // ── Stat boosts (Intimidate, Download, etc.) ──────────────────────────
        EventKind::BoostChanged { target, boost_idx, stages } => {
            if let Some(mon) =
                find_preview_mon(state, &target.player, target.slot_index, slot_map)
                && *boost_idx < 7 {
                    mon.boosts[*boost_idx] = mon.boosts[*boost_idx].saturating_add(*stages);
                }
        }

        EventKind::BoostsCleared { target } => {
            if let Some(mon) =
                find_preview_mon(state, &target.player, target.slot_index, slot_map)
            {
                mon.boosts = [0i8; 7];
            }
        }

        // ── Items ─────────────────────────────────────────────────────────────
        EventKind::ItemRevealed { slot, item } => {
            if let Some(mon) =
                find_preview_mon(state, &slot.player, slot.slot_index, slot_map)
            {
                unknown_set_known(&mut mon.item, item.clone(), "preview-item");
            }
        }

        EventKind::ItemGained { slot, item } => {
            if let Some(mon) =
                find_preview_mon(state, &slot.player, slot.slot_index, slot_map)
            {
                unknown_set_known(&mut mon.item, item.clone(), "preview-item-gained");
            }
        }

        EventKind::ItemLost { slot, item, consumed } => {
            if let Some(mon) =
                find_preview_mon(state, &slot.player, slot.slot_index, slot_map)
            {
                if *consumed {
                    mon.consumed_item = Some(item.clone());
                } else {
                    mon.item_lost = true;
                }
                unknown_set_known(&mut mon.item, Item::None, "preview-item-lost");
            }
        }

        // ── Abilities ─────────────────────────────────────────────────────────
        EventKind::AbilityRevealed { slot, ability } => {
            if let Some(mon) =
                find_preview_mon(state, &slot.player, slot.slot_index, slot_map)
            {
                unknown_set_known(&mut mon.possible_abilities, ability.clone(), "preview-ability");
            }
        }

        // ── Status ────────────────────────────────────────────────────────────
        EventKind::StatusInflicted { target, status } => {
            if let Some(mon) =
                find_preview_mon(state, &target.player, target.slot_index, slot_map)
            {
                mon.status = Some(status.clone());
            }
        }

        // Team-wide status cure (Heal Bell / Aromatherapy) — no-op in team preview
        // (no status is tracked before the battle begins).
        EventKind::TeamStatusCured { .. } => {}

        // ── Forme / type changes (entry abilities like Schooling) ─────────────
        EventKind::FormeChange { slot, into, .. } => {
            if let Some(mon) =
                find_preview_mon(state, &slot.player, slot.slot_index, slot_map)
            {
                unknown_set_known(&mut mon.possible_species, into.clone(), "preview-forme");
            }
        }

        EventKind::TypeChanged { slot, new_types } => {
            if let Some(mon) =
                find_preview_mon(state, &slot.player, slot.slot_index, slot_map)
            {
                unknown_set_known(&mut mon.possible_types, new_types.clone(), "preview-type");
            }
        }

        EventKind::Terastallization { slot, tera_type } => {
            if let Some(mon) =
                find_preview_mon(state, &slot.player, slot.slot_index, slot_map)
            {
                mon.is_tera = true;
                unknown_set_known(&mut mon.possible_tera_type, tera_type.clone(), "preview-tera");
            }
        }

        EventKind::MegaEvolution { slot, into } => {
            if let Some(mon) =
                find_preview_mon(state, &slot.player, slot.slot_index, slot_map)
            {
                mon.is_mega = true;
                unknown_set_known(&mut mon.possible_species, into.clone(), "preview-mega");
            }
        }

        // ── Field effects — no-ops in preview state (no field fields to update)
        // Weather/terrain are set on the BattleState when the battle begins.
        EventKind::WeatherChanged { .. }
        | EventKind::TerrainChanged { .. } => {}

        // ── Illegal events — cannot happen before the first move is chosen ────
        EventKind::MoveUsed { .. }
        | EventKind::Cant { .. }
        | EventKind::ChargingMove { .. }
        | EventKind::MustRecharge { .. }
        | EventKind::SingleMoveOrTurn { .. }
        | EventKind::Crit { .. }
        | EventKind::Missed { .. }
        | EventKind::MoveFailed { .. }
        | EventKind::Blocked { .. }
        | EventKind::Immune { .. }
        | EventKind::HitCount { .. }
        | EventKind::SetHp { .. }
        | EventKind::StatusCured { .. }
        | EventKind::BoostsInverted { .. }
        | EventKind::BoostsSwapped { .. }
        | EventKind::BoostsCopied { .. }
        | EventKind::PseudoWeatherStart { .. }
        | EventKind::PseudoWeatherEnd { .. }
        | EventKind::SideConditionStart { .. }
        | EventKind::SideConditionEnd { .. }
        | EventKind::SlotConditionStart { .. }
        | EventKind::SlotConditionEnd { .. }
        | EventKind::VolatileStart { .. }
        | EventKind::VolatileEnd { .. }
        | EventKind::PerishCount { .. }
        | EventKind::EndOfTurn
        | EventKind::AnticipationShudder { .. }
        | EventKind::IllusionEnded { .. }
        | EventKind::Transformed { .. } => {
            panic!(
                "[inference] illegal event {:?} at team preview",
                event.kind
            );
        }
    }

    for reaction in &event.reactions {
        process_team_preview_event(state, reaction, config, slot_map);
    }
}

// ── Battle path ───────────────────────────────────────────────────────────────

/// Run `pass5_back_solve` on every mon in `state` whose species is fully known.
/// This is a pure information-gain step: it converts tightened `min/max_pre_nature_stat`
/// bounds into narrower `min_evs/max_evs` and excluded natures.  Safe to call multiple
/// times — bounds are monotone so it always converges.
fn run_pass5_all_mons(
    state: &mut UnknownBattleState,
    config: &InferenceConfig,
    dex: &HashMap<Species, PokemonData>,
) {
    let total = mons_count_battle(state);
    for idx in 0..total {
        // S38: skip mons whose EVs are ALREADY fully pinned (`min_evs == max_evs` on
        // every stat) — this is exactly the invariant `from_known_pokemon` establishes
        // for the observer's own (P1) mons, where the true EV is already ground truth
        // and there's nothing to back-solve. Running pass5 anyway is not just wasted
        // work: pass5's own EV_LATTICE search for an EXACT stat target can itself land
        // on a genuine *range* of EVs that floor-round to the same integer stat — and
        // that range shifts when the mon's base stat changes (Mega Evolution, a
        // permanent Forme Change) between two `apply_information` calls (e.g. the
        // team-preview transition, then the turn the mon Mega Evolves). Since pass5's
        // writes are monotone-tightening only, intersecting two genuinely different
        // (base-stat-dependent) EV ranges for the SAME real EV can produce an inverted
        // `min_evs > max_evs` window even though nothing about the mon's real build ever
        // changed — corrupting the exact value `from_known_pokemon` already had right,
        // and eventually crashing pass5's own "every candidate nature is infeasible"
        // soundness assertion on a LATER stat lookup once `min_stats`/`max_stats` get
        // rebuilt from these EVs. A mon this fully known has no business being
        // re-derived by a back-solve meant for opponents in the first place.
        let already_pinned = get_mon_by_idx(state, idx)
            .map(|m| (0..6).all(|si| m.min_evs[si] == m.max_evs[si]))
            .unwrap_or(false);
        if already_pinned {
            continue;
        }
        let has_known_species = get_mon_by_idx(state, idx)
            .map(|m| matches!(m.possible_species, Unknown::Known(_)))
            .unwrap_or(false);
        if has_known_species
            && let Some(mon) = get_mon_mut_by_idx(state, idx) {
                // Captured BEFORE mirroring can overwrite `possible_species` on a
                // `Promoted` outcome — see `finish_illusion_promotion_restore`.
                let discarded_before = mon.possible_species.clone();
                // Mirror onto a live Zoroark hypothesis (Increment 2): this is also the
                // promotion backstop for Pass 3's stat tightening (see the "synergy"
                // note in the plan) — Pass 3 never panics itself, so a primary stat
                // window it silently made infeasible only surfaces HERE, as
                // `pass5_back_solve` panicking on the primary while the hypothesis
                // (independently tightened by Pass 3's own mirrored call) still solves
                // cleanly.
                let outcome = apply_with_illusion_mirroring(mon, |m| pass5_back_solve(m, config, dex));
                if matches!(outcome, IllusionMirrorOutcome::Promoted) {
                    let side = if mon_is_p2(state, idx) { Player::P2 } else { Player::P1 };
                    resolve_zoroark_globally(state, side);
                    finish_illusion_promotion_restore(state, side, discarded_before, dex, config);
                }
            }
    }
}

fn apply_information_battle(
    state: &mut UnknownBattleState,
    events: &[InformationEvent],
    dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    ability_dex: &HashMap<Ability, AbilityData>,
    config: &InferenceConfig,
) {
    // Whole-turn breadcrumb for `inference_contradiction!` (see `CURRENT_EVENT_CONTEXT`
    // doc comment) — covers Pass 4 and the tail BCP passes below, which aren't tied to
    // any single event. `process_battle_event` narrows this to a specific node's
    // `EventKind` once the per-event walk starts.
    CURRENT_EVENT_CONTEXT.with(|c| {
        *c.borrow_mut() = Some(format!(
            "turn=[{}]",
            events
                .iter()
                .map(|e| format!("{:?}", e.kind))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    });

    // S32: snapshot of `state` as of turn start, BEFORE the event walk mutates field
    // conditions (weather/terrain/Trick Room/side conditions/boosts/status). Both
    // `pass4_speed_from_order` calls below seed their running speed-relevant trackers
    // from this snapshot — never from `state` at call time — so the *second* call
    // (after the walk, once `state` reflects end-of-turn field conditions) doesn't
    // misattribute a mid-turn or end-of-turn field change to pairings that raced
    // before it existed. See `pass4_speed_from_order`'s doc comment.
    let seed_state = state.clone();

    // ── Pass 4 (first): speed ordering → Spe bounds ─────────────────────────
    // Run BEFORE the event walk so that speed bounds (min_stats[5]/max_stats[5])
    // are already tightened when Pass 3 calls the damage oracle for Gyro Ball
    // and Electro Ball, which compute BP from effective speeds.
    pass4_speed_from_order(state, &seed_state, events, dex, move_dex, ability_dex);
    // Propagate the emitted SpeedComparison predicates to fixpoint immediately.
    // Predicate set is static here (no clause pruning), so collect once and reuse.
    {
        let sc = bcp::collect_speed_comparisons(state);
        while bcp::propagate_collected(state, &sc) {}
    }

    // ── Pass 1–3: depth-first event walk ─────────────────────────────────────
    let mut ctx = BattleContext {
        dex,
        move_dex,
        ability_dex,
        config,
        move_context: None,
        switch_slot: None,
        damaging_hits_this_turn: Vec::new(),
        move_users_this_turn: Vec::new(),
        analytic_last_movers: compute_analytic_last_movers(events),
        turn_segment: 0,
    };
    for event in events {
        process_battle_event(state, event, &mut ctx);
    }

    // Reset the breadcrumb to the whole-turn view now that the per-event walk has
    // finished. `process_battle_event` narrowed it to each node's `EventKind` as it
    // went (see above) and never resets it — left alone, any contradiction raised by
    // Pass 5, Pass 6 (BCP), or the Pass 4 re-derivation below would misleadingly
    // report `event=<whatever the turn's last event was>` (frequently `EndOfTurn` or
    // a `VolatileEnd`), even though none of those passes are examining that specific
    // event. This was the source of confusing `event=VolatileEnd`/`event=EndOfTurn`
    // labels on speed-comparison contradictions that Pass 4 actually raised from
    // unrelated `MoveUsed` pairings (S32).
    CURRENT_EVENT_CONTEXT.with(|c| {
        *c.borrow_mut() = Some(format!(
            "post-walk turn=[{}]",
            events
                .iter()
                .map(|e| format!("{:?}", e.kind))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    });

    // ── Pass 5 (first): back-solve EV/IV/nature from tightened stat bounds ───
    run_pass5_all_mons(state, config, dex);

    // ── Pass 6: BCP to fixpoint ────────────────────────────────────────────────
    bcp::run_bcp(state, config.allow_repeat_items, dex, config);

    // ── Pass 4 re-derivation: if BCP forced a priority ability to Known, re-run
    // speed ordering with the tighter bracket so speed bounds are updated.
    // One re-run is sufficient; duplicate clauses are now guarded against.
    pass4_speed_from_order(state, &seed_state, events, dex, move_dex, ability_dex);
    {
        let sc = bcp::collect_speed_comparisons(state);
        while bcp::propagate_collected(state, &sc) {}
    }
    bcp::run_bcp(state, config.allow_repeat_items, dex, config);

    // ── Pass 5 (second): re-run after BCP so that stat bounds tightened by
    // force_literal (e.g. from a SpeedComparison clause resolving to Known) are
    // reflected in EV/IV/nature narrowing.  BCP is re-run once more to propagate
    // any newly excluded natures.  Bounds are monotone → guaranteed to terminate.
    run_pass5_all_mons(state, config, dex);
    bcp::run_bcp(state, config.allow_repeat_items, dex, config);
}

/// Context threaded through the recursive event walk.
struct BattleContext<'a> {
    dex: &'a HashMap<Species, PokemonData>,
    move_dex: &'a HashMap<PokemonMove, MoveData>,
    ability_dex: &'a HashMap<Ability, AbilityData>,
    config: &'a InferenceConfig,
    /// The nearest enclosing `MoveUsed`, for nested-reaction analysis.
    move_context: Option<MoveContext>,
    /// The nearest enclosing single-mon `Switch`, set while processing that event's
    /// reactions.  Used by `WeatherChanged` / `TerrainChanged` handlers to attribute
    /// ability-triggered field effects (Drizzle, Drought, etc.) to the switching mon.
    switch_slot: Option<FieldSlot>,
    /// Per-turn record of damaging hits: (attacker_slot, target_slot, move_used).
    /// One entry per distinct (attacker, target) pair that produced a DamageDealt to a
    /// non-self target.  Populated in the MoveUsed block; cleared at EndOfTurn.
    /// Consulted by the Cant{Flinch} handler to attribute King's Rock / Razor Fang / Stench.
    damaging_hits_this_turn: Vec<(FieldSlot, FieldSlot, PokemonMove)>,
    /// Ordered list of user slots that have executed a MoveUsed event so far this turn.
    /// Populated after each MoveUsed's Pass 3 runs; cleared at EndOfTurn.
    /// (Retained for potential future use; Analytic now uses `analytic_last_movers`.)
    move_users_this_turn: Vec<FieldSlot>,
    /// S28: per-turn-segment last move-committed actor — the slot for which Analytic's
    /// "moved last" fires. Computed once from the event stream (`compute_analytic_last_movers`),
    /// indexed by `turn_segment`. Replaces the old "did the target already move this turn?"
    /// heuristic, which was wrong whenever the target switched, flinched, or was fully
    /// paralyzed (and was the wrong predicate in doubles regardless).
    analytic_last_movers: Vec<Option<FieldSlot>>,
    /// S28: index into `analytic_last_movers`; advanced at each `EndOfTurn`.
    turn_segment: usize,
}

#[derive(Clone)]
struct MoveContext {
    user_slot: FieldSlot,
    pokemon_move: PokemonMove,
    targets: Vec<FieldSlot>,
    is_crit: bool,
    /// Pre-move HP of each target (for Pass 3 damage delta).
    pre_hit_hp: Vec<(FieldSlot, PokemonHP)>,
    /// Accumulated observed damage intervals per target, in hit order.
    observed_damage: Vec<(FieldSlot, PokemonHP)>,
    /// S24: full fog snapshot of the attacker as of the moment the move was declared.
    /// Pass 1 applies the whole reaction tree (self-boosts, Flame Body burns, item
    /// consumption) before Pass 3 runs, so the live mon reflects POST-move state for
    /// an observation made PRE-move. Pass 3 enumerates items/abilities and
    /// materializes from this snapshot; bounds still write back to the live mon.
    pre_move_attacker: Option<UnknownPokemonState>,
    /// S24: pre-move fog snapshot of each target (same rationale — e.g. a Def-drop
    /// secondary or a resist berry consumed by the observed hit itself must not leak
    /// into the oracle run for that hit).
    pre_move_targets: Vec<(FieldSlot, UnknownPokemonState)>,
}

/// S28: for each turn segment (split on top-level `EndOfTurn`), the slot of the last
/// move-committed actor — the slot for which Analytic's "moved last" fires.
///
/// A slot commits a move (occupies a `MoveAction` in the sim's queue) via a top-level
/// `MoveUsed`, `Cant`, `MustRecharge`, `ChargingMove`, or `SingleMoveOrTurn`; `Switch`
/// does not. If the segment's last commit wasn't a `MoveUsed` (e.g. the last mon
/// flinched), Analytic fires for nobody that segment.
fn compute_analytic_last_movers(top_events: &[InformationEvent]) -> Vec<Option<FieldSlot>> {
    let mut segments: Vec<Option<FieldSlot>> = Vec::new();
    let mut last: Option<FieldSlot> = None;
    for e in top_events {
        match &e.kind {
            EventKind::MoveUsed { user, .. }
            | EventKind::ChargingMove { user, .. } => last = Some(*user),
            EventKind::Cant { slot, .. }
            | EventKind::MustRecharge { slot }
            | EventKind::SingleMoveOrTurn { slot, .. } => last = Some(*slot),
            EventKind::EndOfTurn => segments.push(last.take()),
            _ => {}
        }
    }
    segments.push(last); // trailing segment (event list need not end at EndOfTurn)
    segments
}

/// Depth-first event walk applying Passes 1–3.
fn process_battle_event(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    ctx: &mut BattleContext,
) {
    // Narrow the `inference_contradiction!` breadcrumb to this specific node while it's
    // being resolved (see `CURRENT_EVENT_CONTEXT` doc comment above the macro).
    CURRENT_EVENT_CONTEXT.with(|c| {
        *c.borrow_mut() = Some(format!("{:?}", event.kind));
    });

    let prev_move_ctx = ctx.move_context.clone();
    let prev_switch_slot = ctx.switch_slot;

    // Detect crit in the reaction list for the MoveContext.
    let is_crit = event
        .reactions
        .iter()
        .any(|r| matches!(r.kind, EventKind::Crit { .. }));

    if let EventKind::MoveUsed {
        user,
        move_used,
        targets,
    } = &event.kind
    {
        // Snapshot pre-hit HP for all targets (Pass 3 scaffold).
        let pre_hit_hp = targets
            .iter()
            .filter_map(|t| {
                mon_idx_for_active_slot(state, t)
                    .and_then(|i| get_mon_by_idx(state, i))
                    .map(|m| (*t, m.hp.clone()))
            })
            .collect();

        // S24: full pre-move fog snapshots for Pass 3 (see MoveContext field docs).
        let pre_move_attacker = mon_idx_for_active_slot(state, user)
            .and_then(|i| get_mon_by_idx(state, i))
            .cloned();
        let pre_move_targets = targets
            .iter()
            .filter_map(|t| {
                mon_idx_for_active_slot(state, t)
                    .and_then(|i| get_mon_by_idx(state, i))
                    .map(|m| (*t, m.clone()))
            })
            .collect();

        ctx.move_context = Some(MoveContext {
            user_slot: *user,
            pokemon_move: move_used.clone(),
            targets: targets.clone(),
            is_crit,
            pre_hit_hp,
            observed_damage: Vec::new(),
            pre_move_attacker,
            pre_move_targets,
        });
        ctx.switch_slot = None;

        // Accumulate damaging hits for the Cant{Flinch} deduction.
        // One entry per distinct (attacker, target) pair — multi-hit moves are deduped.
        // Self-damage (Life Orb recoil, crash) is excluded via `target != user`.
        for reaction in &event.reactions {
            if let EventKind::DamageDealt { target, .. } = &reaction.kind
                && target != user {
                    let already_recorded = ctx
                        .damaging_hits_this_turn
                        .iter()
                        .any(|(a, t, _)| a == user && t == target);
                    if !already_recorded {
                        ctx.damaging_hits_this_turn
                            .push((*user, *target, move_used.clone()));
                    }
                }
        }
    }

    // For a single-mon switch, record the slot so that field-effect reactions
    // (WeatherChanged / TerrainChanged from Drizzle / Electric Surge, etc.)
    // can attribute the effect to the switching mon.
    if let EventKind::Switch(sw) = &event.kind {
        ctx.switch_slot = Some(sw.slot);
    }

    pass1_apply_event(state, event, ctx);

    for reaction in &event.reactions {
        process_battle_event(state, reaction, ctx);
    }

    // Pass 2/3 — item and stat inference keyed on the full MoveUsed + reactions.
    if let EventKind::MoveUsed { user, .. } = &event.kind {
        let user_slot_for_order = *user;
        pass2_item_from_move(state, event, ctx);
        pass2_contact_absence(state, event, ctx);
        pass2_prankster_immunity(state, event, ctx);
        pass2_powder_immunity(state, event, ctx);
        pass2_guaranteed_status_absence(state, event, ctx);
        pass2_ground_immune_clause(state, event, ctx);
        pass3_damage_to_stats(state, event, ctx);
        // Recorded after Pass 3 so this move's own oracle calls don't see the user in
        // the list yet (vestigial ordering; Analytic itself now uses analytic_last_movers).
        ctx.move_users_this_turn.push(user_slot_for_order);
    }

    // Clear per-turn accumulators at the boundary of each turn.
    if matches!(event.kind, EventKind::EndOfTurn) {
        ctx.damaging_hits_this_turn.clear();
        ctx.move_users_this_turn.clear();
        // S28: advance to the next turn segment's precomputed last-mover.
        ctx.turn_segment += 1;
    }

    ctx.move_context = prev_move_ctx;
    ctx.switch_slot = prev_switch_slot;
}

// ── Pass 1: Direct/structural facts ──────────────────────────────────────────

/// Thin dispatcher: routes each `EventKind` to the appropriate Pass-1 handler group.
fn pass1_apply_event(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    ctx: &BattleContext,
) {
    match &event.kind {
        EventKind::Switch(_) | EventKind::SimultaneousSwitch { .. } => {
            pass1_apply_switch_event(state, event, ctx);
        }

        EventKind::EndOfTurn => {
            apply_end_of_turn(state, event);
            pass_eot_heal(state, event, ctx);
            pass_eot_sand_immunity(state, event, ctx);
            pass_eot_self_status(state, event, ctx);
        }

        EventKind::MoveUsed {
            user, move_used, ..
        } => {
            // Struggle is not a moveslot — it fires only when every move is out of PP
            // (or the holder is Choice-locked into a 0-PP move) and is not a real move
            // choice. The simulator itself excludes Struggle from choice-lock, last-move,
            // and Copycat bookkeeping (simulator/mod.rs); mirror that here. Without this
            // guard, `reveal_move_on_mon` would burn a real moveslot on a non-move, and
            // `pass1_choice_exclusion` would see "two distinct moves used" — Choice's own
            // failure mode — and unsoundly exclude Choice items in exactly the scenario
            // that causes it.
            if *move_used == PokemonMove::Struggle {
                return;
            }
            // Set when the move-legality mirroring below resolves this mon's Zoroark
            // hypothesis (the primary was infeasible, the hypothesis wasn't) — acted on
            // AFTER the mon borrow ends, since `resolve_zoroark_globally` needs `state`.
            let mut promoted_illusion = false;
            let mut discarded_before: Unknown<Species> = Unknown::Not(Vec::new());
            if let Some(idx) = mon_idx_for_active_slot(state, user)
                && let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    // Captured BEFORE mirroring can overwrite `possible_species` on a
                    // `Promoted` outcome — see `finish_illusion_promotion_restore`.
                    discarded_before = mon.possible_species.clone();
                    let learnset_dex = &ctx.config.learnset_dex;
                    let outcome = apply_with_illusion_mirroring(mon, |m| {
                        reveal_move_on_mon(m, move_used);
                        check_move_legal_for_species(m, move_used, learnset_dex);
                    });
                    promoted_illusion = matches!(outcome, IllusionMirrorOutcome::Promoted);
                    narrow_species_by_learnset(
                        mon, move_used, &ctx.config.learnset_dex, ctx.dex,
                    );
                    // Matches the sim's 0-based streak convention exactly
                    // (simulator/mod.rs: `new_count = if last_used_move == move { count+1 }
                    // else { 0 }`): a move's first use in a streak is count 0, not 1.
                    if Some(move_used) == mon.last_used_move.as_ref() {
                        mon.consecutive_move_count = mon.consecutive_move_count.saturating_add(1);
                    } else {
                        mon.consecutive_move_count = 0;
                    }
                    // Update used_moves_this_field BEFORE choice exclusion (it reads it).
                    for i in 0..4 {
                        if mon.known_moves[i] == Some(move_used.clone()) {
                            mon.used_moves_this_field[i] = true;
                        }
                    }
                    // Choice-item exclusion: keyed on used_moves_this_field (not last_used_move
                    // which survives switch-out).
                    pass1_choice_exclusion(mon, move_used);
                    mon.last_used_move = Some(move_used.clone());

                    // S27: mirror the sim's zero-effective-damage reset (simulator/mod.rs:
                    // total_effective_dmg == 0 → count = 0, last_used_move = None). A damaging
                    // move dealing no damage to any non-user target (miss/immune/blocked) breaks
                    // the Metronome streak in the sim; otherwise the fog streak drifts upward and
                    // feeds straight into the Pass 3 oracle for our own attacker (Direction A).
                    let is_damaging = ctx
                        .move_dex
                        .get(move_used)
                        .is_some_and(|md| !matches!(md.category, MoveCategory::Status));
                    let dealt_damage = event.reactions.iter().any(|r| {
                        matches!(&r.kind, EventKind::DamageDealt { target, .. } if target != user)
                    });
                    if is_damaging && !dealt_damage {
                        mon.consecutive_move_count = 0;
                        mon.last_used_move = None;
                    }
                }
            // Field-level last-move tracker for Copycat (simulator/mod.rs sets this
            // unconditionally for any executed non-Struggle move, on the top-level state,
            // not per-mon — set after the mon borrow above ends).
            state.last_move_on_field = Some(move_used.clone());
            if promoted_illusion {
                resolve_zoroark_globally(state, user.player);
                finish_illusion_promotion_restore(state, user.player, discarded_before, ctx.dex, ctx.config);
            }
        }

        EventKind::Faint { slot } => {
            if let Some(idx) = mon_idx_for_active_slot(state, slot)
                && let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    mon.fainted = true;
                    mon.hp = PokemonHP::Percent(0);
                }
        }

        EventKind::DamageDealt { target, new_hp, .. } => {
            let old_hp = mon_idx_for_active_slot(state, target)
                .and_then(|i| get_mon_by_idx(state, i))
                .map(|m| m.hp.clone());
            update_mon_hp(state, target, new_hp.clone());

            // Belt-and-braces faint detection: the display convention (hp_to_percent)
            // shows 0 only at an actual faint, so DamageDealt-to-0 implies fainted even
            // if the explicit Faint event is missing. Keeps fainted-guards in the EOT
            // passes (and ability-suppression scans) sound.
            if matches!(new_hp, PokemonHP::Number(0) | PokemonHP::Percent(0))
                && let Some(idx) = mon_idx_for_active_slot(state, target)
                    && let Some(mon) = get_mon_mut_by_idx(state, idx) {
                        mon.fainted = true;
                    }

            // Compute the HP delta (amount of damage dealt, not the pre-hit HP value).
            // The simulator stores eff_damage (the delta) in last_damage_taken; we must
            // match that so Counter / Mirror Coat / Metal Burst work correctly.
            let damage_delta: PokemonHP = match (&old_hp, &new_hp) {
                (Some(PokemonHP::Number(o)), PokemonHP::Number(n)) => {
                    PokemonHP::Number(o.saturating_sub(*n))
                }
                (Some(PokemonHP::Percent(o)), PokemonHP::Percent(n)) => {
                    PokemonHP::Percent(o.saturating_sub(*n))
                }
                _ => PokemonHP::Percent(0),
            };

            // Per-turn damage tracking (mirrors end_turn Phase 5 fields).
            if let Some(idx) = mon_idx_for_active_slot(state, target)
                && let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    mon.damaged_this_turn = true;
                    mon.last_damage_taken = damage_delta.clone();

                    // Attribute to the enclosing MoveUsed if available.
                    if let Some(ref mctx) = ctx.move_context {
                        let attacker = &mctx.user_slot;
                        if !mon.damaged_by_this_turn.contains(attacker) {
                            mon.damaged_by_this_turn.push(*attacker);
                        }
                        mon.last_damage_attacker = Some(*attacker);
                        if let Some(md) = ctx.move_dex.get(&mctx.pokemon_move) {
                            match md.category {
                                MoveCategory::Physical => {
                                    mon.last_physical_damage_taken = damage_delta.clone();
                                    mon.last_physical_attacker = Some(*attacker);
                                }
                                MoveCategory::Special => {
                                    mon.last_special_damage_taken = damage_delta.clone();
                                    mon.last_special_attacker = Some(*attacker);
                                }
                                MoveCategory::Status => {}
                            }
                            // Rage Fist counter: increments only for a Physical/Special hit on
                            // a non-user target — never the move's own recoil/crash self-damage
                            // (separate path), Status moves, or EOT/residual chip (no enclosing
                            // MoveUsed; handled by the `None` arm below).
                            if target != attacker
                                && matches!(md.category, MoveCategory::Physical | MoveCategory::Special)
                            {
                                mon.times_hit = mon.times_hit.saturating_add(1);
                            }
                        }
                    }
                }
        }
        EventKind::Healed { target, new_hp, .. } => {
            update_mon_hp(state, target, new_hp.clone());
        }
        EventKind::SetHp { target, new_hp, .. } => {
            update_mon_hp(state, target, new_hp.clone());
        }

        EventKind::StatusInflicted { target, status } => {
            if let Some(idx) = mon_idx_for_active_slot(state, target)
                && let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    if let Some(ref existing) = mon.status.clone()
                        && existing != status {
                            inference_contradiction!(
                                idx,
                                "StatusInflicted {:?} but already has {:?}",
                                status,
                                existing
                            );
                        }
                    mon.status = Some(status.clone());
                }
        }

        EventKind::StatusCured { target, .. } => {
            if let Some(idx) = mon_idx_for_active_slot(state, target)
                && let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    mon.status = None;
                }
        }

        // Heal Bell / Aromatherapy: cure the entire side including benched mons.
        EventKind::TeamStatusCured { side } => {
            let total = mons_count_battle(state);
            // S1: a side's roster is not one contiguous mon_idx range under the
            // active-segments-first layout (P1's bench sits after P2's active
            // segment) — filter every index by side membership instead.
            for idx in 0..total {
                let is_p2 = mon_is_p2(state, idx);
                let matches_side = match side {
                    Player::P1 => !is_p2,
                    Player::P2 => is_p2,
                };
                if matches_side
                    && let Some(mon) = get_mon_mut_by_idx(state, idx) {
                        mon.status = None;
                    }
            }
        }

        EventKind::ItemRevealed { slot, item } => {
            if let Some(idx) = mon_idx_for_active_slot(state, slot) {
                if let Some(legal) = &ctx.config.legal_items
                    && !legal.contains(item) && *item != Item::None {
                        inference_contradiction!(
                            idx,
                            "ItemRevealed {:?} outside legal whitelist",
                            item
                        );
                    }
                // Item clause: a confirmed team-built item cannot be held by any other
                // roster member on the same side — but ONLY when this mon's own item is
                // itself team-built. A mon carrying a transferred item (Trick/Switcheroo/
                // Symbiosis/Recycle/Pickup — item_was_transferred) reveals nothing about
                // what this mon's team built, so a later ItemRevealed re-confirming that
                // transferred item must not exclude it from this mon's teammates (S12:
                // e.g. Frisk revealing a foe's Tricked-in item).
                let was_transferred = get_mon_by_idx(state, idx)
                    .is_some_and(|m| m.item_was_transferred);
                let mut promoted_illusion = false;
                let mut discarded_before: Unknown<Species> = Unknown::Not(Vec::new());
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    // Captured BEFORE mirroring can overwrite `possible_species` on a
                    // `Promoted` outcome — see `finish_illusion_promotion_restore`.
                    discarded_before = mon.possible_species.clone();
                    // `unknown_set_known` panics on any conflict (unlike ability reveal,
                    // which has its own "live change" overwrite path) — mirror it onto a
                    // live Zoroark hypothesis so an item inconsistent with the
                    // hypothesis' own item bound drops it. `enforce_unique_item` below is
                    // a whole-side, cross-teammate side effect and stays primary-only —
                    // `apply_with_illusion_mirroring`'s contract requires the mirrored
                    // closure to only touch `mon` itself.
                    let outcome = apply_with_illusion_mirroring(mon, |m| {
                        unknown_set_known(&mut m.item, item.clone(), &format!("mon#{idx} item"));
                    });
                    promoted_illusion = matches!(outcome, IllusionMirrorOutcome::Promoted);
                }
                if !was_transferred {
                    enforce_unique_item(state, idx, item, ctx.config.allow_repeat_items);
                }
                if promoted_illusion {
                    resolve_zoroark_globally(state, slot.player);
                    finish_illusion_promotion_restore(state, slot.player, discarded_before, ctx.dex, ctx.config);
                }
            }
        }
        EventKind::ItemGained { slot, item } => {
            // NOTE: ItemGained covers mid-battle item transfers (Trick, Switcheroo,
            // Recycle, Pickup). These are not team-built items, so item-clause
            // exclusion must NOT propagate to teammates here.
            if let Some(idx) = mon_idx_for_active_slot(state, slot) {
                if let Some(legal) = &ctx.config.legal_items
                    && !legal.contains(item) && *item != Item::None {
                        inference_contradiction!(
                            idx,
                            "ItemRevealed {:?} outside legal whitelist",
                            item
                        );
                    }
                // S19: capture the outgoing item before it is overwritten, so the
                // stale HasItem clauses about this mon can be resolved historically.
                let outgoing = get_mon_by_idx(state, idx).and_then(|m| match &m.item {
                    Unknown::Known(i) => Some(i.clone()),
                    _ => None,
                });
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    mon.item = Unknown::Known(item.clone());
                    mon.item_lost = false;
                    // Marks this mon's held item as no-longer-team-built, so any future
                    // ItemRevealed re-confirming it skips the item-clause exclusion (S12).
                    mon.item_was_transferred = true;
                }
                resolve_item_clauses_on_item_change(state, idx, outgoing);
            }
        }
        EventKind::ItemLost {
            slot,
            item,
            consumed,
        } => {
            if let Some(idx) = mon_idx_for_active_slot(state, slot) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    if *consumed {
                        mon.consumed_item = Some(item.clone());
                    } else {
                        mon.item_lost = true;
                        // Preserve the named item revealed by Knock Off / Thief / Fling.
                        mon.removed_item = Some(item.clone());
                    }
                    mon.item = Unknown::Known(Item::None);
                }
                // S19: the held item just changed — persisted HasItem clauses about
                // this mon describe the item that was consumed/removed (named by the
                // event), not the now-empty slot.
                resolve_item_clauses_on_item_change(state, idx, Some(item.clone()));
            }
        }

        EventKind::AbilityRevealed { slot, ability } => {
            if let Some(idx) = mon_idx_for_active_slot(state, slot)
                && let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    // Narrow-vs-overwrite: if the revealed ability is still possible, narrow
                    // via `unknown_set_known`. If it's excluded under any `Unknown`
                    // representation (outside `Possibly`, in a `Not` list, or a different
                    // `Known` value), a live ability change occurred (Trace, Skill Swap,
                    // Mummy, Wandering Spirit, …) — overwrite instead of treating it as a
                    // contradiction. `possible_original_abilities` is untouched either way;
                    // it tracks the innate ability, which never changes here. S14: previously
                    // only the `Possibly` case overwrote; `Not`-excluded and `Known(other)`
                    // wrongly panicked despite being the same live-change scenario.
                    if unknown_is_excluded(&mon.possible_abilities, ability) {
                        mon.possible_abilities = Unknown::Known(ability.clone());
                    } else {
                        unknown_set_known(
                            &mut mon.possible_abilities,
                            ability.clone(),
                            &format!("mon#{idx} ability"),
                        );
                    }
                }
        }

        EventKind::BoostChanged {
            target,
            boost_idx,
            stages,
        } => {
            if let Some(idx) = mon_idx_for_active_slot(state, target)
                && let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    if *boost_idx < 7 {
                        let new_stage =
                            (mon.boosts[*boost_idx] as i16 + *stages as i16).clamp(-6, 6) as i8;
                        mon.boosts[*boost_idx] = new_stage;
                    }
                    if *stages > 0 {
                        mon.stats_raised_this_turn = true;
                    } else if *stages < 0 {
                        mon.stats_lowered_this_turn = true;
                    }
                }
        }
        EventKind::BoostsCleared { target } => {
            if let Some(idx) = mon_idx_for_active_slot(state, target)
                && let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    mon.boosts = [0; 7];
                }
        }
        EventKind::BoostsInverted { target } => {
            if let Some(idx) = mon_idx_for_active_slot(state, target)
                && let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    for b in mon.boosts.iter_mut() {
                        *b = -*b;
                    }
                }
        }
        EventKind::BoostsSwapped { source, target } => {
            let src_idx = mon_idx_for_active_slot(state, source);
            let tgt_idx = mon_idx_for_active_slot(state, target);
            if let (Some(si), Some(ti)) = (src_idx, tgt_idx) {
                let sb = get_mon_by_idx(state, si).map(|m| m.boosts);
                let tb = get_mon_by_idx(state, ti).map(|m| m.boosts);
                if let (Some(sb), Some(tb)) = (sb, tb) {
                    if let Some(sm) = get_mon_mut_by_idx(state, si) {
                        sm.boosts = tb;
                    }
                    if let Some(tm) = get_mon_mut_by_idx(state, ti) {
                        tm.boosts = sb;
                    }
                }
            }
        }
        EventKind::BoostsCopied { source, target } => {
            let src_idx = mon_idx_for_active_slot(state, source);
            let tgt_idx = mon_idx_for_active_slot(state, target);
            if let (Some(si), Some(ti)) = (src_idx, tgt_idx) {
                let sb = get_mon_by_idx(state, si).map(|m| m.boosts);
                if let (Some(sb), Some(tm)) = (sb, get_mon_mut_by_idx(state, ti)) {
                    tm.boosts = sb;
                }
            }
        }

        EventKind::MegaEvolution { slot, into } => {
            if let Some(idx) = mon_idx_for_active_slot(state, slot) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    // Overwrite (not unknown_set_known): MegaEvolution genuinely changes
                    // the mon's apparent species away from its pre-mega Known value, so
                    // the "already Known to something else" panic path is not a
                    // contradiction here — it's the expected common case. Mirrors the
                    // IllusionEnded handler's overwrite pattern.
                    mon.possible_species = Unknown::Known(into.clone());
                    mon.is_mega = true;
                    // Update types, ability set, and weight from the mega species dex entry.
                    if let Some(data) = ctx.dex.get(into) {
                        mon.possible_types = Unknown::Known(data.types.clone());
                        mon.possible_weight_hg = Unknown::Known(data.weight);
                        // Mega abilities are fixed per mega species — recompute both
                        // original and live ability to the mega's slot set.
                        let mega_abilities = if data.abilities.is_empty() {
                            Unknown::Not(Vec::new())
                        } else {
                            Unknown::Possibly(data.abilities.clone())
                        };
                        mon.possible_original_abilities = mega_abilities.clone();
                        mon.possible_abilities = mega_abilities;
                        // Mega Evolution swaps in a new base-stat table; EVs/IVs/nature are
                        // unaffected, but the achievable final-stat window must be remapped
                        // against the new base or pass5 will see an impossible window
                        // (bounds computed for the pre-mega base stats).
                        recompute_stat_bounds_for_species_change(mon, data.base_stats, mon.level);
                    }
                }
                // A mega's base-stat table differs from the pre-mega species', so any
                // predicate whose numeric content was derived against the OLD base
                // stats (SpeedComparison, EVIVStatGE/LE, nature-direction, threatening-
                // move clauses) is stale the instant `recompute_stat_bounds_for_species_change`
                // above remaps `min_stats`/`max_stats` to the new table — a persisted
                // pre-mega SpeedComparison capping `max_stats[5]` below the freshly
                // widened `min_stats[5]` is exactly the "SpeedComparison raises min above
                // max" contradiction this purge prevents. Same purge as S30's
                // `IllusionEnded` handler (`statement_stale_after_species_reveal`);
                // dropping a predicate only widens the fog, so this is sound.
                state.predicates.retain(|clause| {
                    !clause
                        .iter()
                        .any(|lit| statement_stale_after_species_reveal(lit, idx))
                });
                match slot.player {
                    // p1_has_mega / p2_has_mega means "resource still available" —
                    // initialized true, flipped to false when the Mega is used.
                    Player::P1 => state.p1_has_mega = false,
                    Player::P2 => state.p2_has_mega = false,
                }
                // Pin the held item to the required Mega Stone.
                let mega_stone = ctx
                    .dex
                    .get(into)
                    .and_then(|d| d.required_item.as_ref().map(|s| Item::from_str(s)));
                if let Some(stone) = mega_stone
                    && stone != Item::None {
                        if let Some(legal) = &ctx.config.legal_items
                            && !legal.contains(&stone) {
                                inference_contradiction!(
                                    idx,
                                    "Mega Stone {:?} outside legal whitelist",
                                    stone
                                );
                            }
                        if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                            unknown_set_known(
                                &mut mon.item,
                                stone,
                                &format!("mon#{idx} mega-stone"),
                            );
                        }
                    }
            }
        }

        EventKind::Terastallization { slot, tera_type } => {
            if let Some(idx) = mon_idx_for_active_slot(state, slot) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    mon.is_tera = true;
                    unknown_set_known(
                        &mut mon.possible_tera_type,
                        tera_type.clone(),
                        &format!("mon#{idx} tera"),
                    );
                }
                match slot.player {
                    Player::P1 => state.p1_has_tera = false,
                    Player::P2 => state.p2_has_tera = false,
                }
            }
        }

        EventKind::FormeChange { slot, into, .. } => {
            if let Some(idx) = mon_idx_for_active_slot(state, slot) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    // Overwrite (not unknown_set_known): a forme change (Stance Change,
                    // Mimikyu-Busted, Palafin-Hero, etc.) genuinely changes the apparent
                    // species away from its pre-change Known value — see MegaEvolution above.
                    mon.possible_species = Unknown::Known(into.clone());
                    if let Some(data) = ctx.dex.get(into) {
                        mon.possible_types = Unknown::Known(data.types.clone());
                        mon.possible_weight_hg = Unknown::Known(data.weight);
                        // Forme-change abilities are fixed per forme — recompute ability
                        // sets to the new forme's slot set.
                        let forme_abilities = if data.abilities.is_empty() {
                            Unknown::Not(Vec::new())
                        } else {
                            Unknown::Possibly(data.abilities.clone())
                        };
                        mon.possible_original_abilities = forme_abilities.clone();
                        mon.possible_abilities = forme_abilities;
                        // Forme changes (Stance Change, Mimikyu-Busted, …) can swap in a
                        // different base-stat table — remap the achievable-stat window
                        // the same way Mega Evolution does (see above).
                        recompute_stat_bounds_for_species_change(mon, data.base_stats, mon.level);
                    }
                }
                // Same stale-predicate purge as MegaEvolution above (and S30's
                // IllusionEnded handler) — a forme change that swaps base stats
                // invalidates any SpeedComparison/EVIVStatGE/LE/nature-direction clause
                // derived against the old table.
                state.predicates.retain(|clause| {
                    !clause
                        .iter()
                        .any(|lit| statement_stale_after_species_reveal(lit, idx))
                });
            }
        }

        EventKind::TypeChanged { slot, new_types } => {
            if let Some(idx) = mon_idx_for_active_slot(state, slot)
                && let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    mon.possible_types = Unknown::Known(new_types.clone());
                }
        }

        EventKind::WeatherChanged { weather } => {
            // Any weather change invalidates pending turn-count clauses for the old
            // weather (they refer to a timer that no longer exists).
            state.predicates.retain(|c| {
                !c.iter().any(|l| matches!(l, Statement::WeatherTurns { .. }))
            });
            state.weather = weather.clone();
            state.weather_turns = weather.as_ref().map(weather_timer);
            // I-A: record the setter so the rock item can be revealed when the timer
            // collapses from Possibly([5,8]) to Known(3) after 5 end-of-turns.
            state.weather_setter_mon_idx = if let Some(mctx) = &ctx.move_context {
                // Move-triggered weather (Rain Dance, Sunny Day, …) — setter is move user.
                mon_idx_for_active_slot(state, &mctx.user_slot)
            } else if let Some(sw_slot) = &ctx.switch_slot {
                // Ability-triggered weather on single-mon switch-in (Drizzle, Drought, …).
                mon_idx_for_active_slot(state, sw_slot)
            } else {
                None // SimultaneousSwitch or other; setter unknown.
            };
            // Turn-count CNF pair: tie the setter's extension rock to the duration so
            // BCP propagates in BOTH directions (an item reveal collapses the timer;
            // a collapsed timer resolves the item).
            if let (Some(setter), Some(w)) = (state.weather_setter_mon_idx, weather.as_ref())
                && matches!(&state.weather_turns, Some(Unknown::Possibly(_)))
                    && let Some(rock) = weather_extension_item(w)
                        && ctx.config.legal_item_ok(&rock) {
                            state.predicates.push(vec![
                                Statement::HasItem { mon_idx: setter, item: rock.clone() },
                                Statement::WeatherTurns { turns: 5 },
                            ]);
                            state.predicates.push(vec![
                                Statement::Not(Box::new(Statement::HasItem {
                                    mon_idx: setter,
                                    item: rock,
                                })),
                                Statement::WeatherTurns { turns: 8 },
                            ]);
                        }
        }
        EventKind::TerrainChanged { terrain } => {
            state.predicates.retain(|c| {
                !c.iter().any(|l| matches!(l, Statement::TerrainTurns { .. }))
            });
            state.terrain = terrain.clone();
            state.terrain_turns = terrain.as_ref().map(terrain_timer);
            // I-A: record the setter for TerrainExtender reveal on timer collapse.
            state.terrain_setter_mon_idx = if let Some(mctx) = &ctx.move_context {
                mon_idx_for_active_slot(state, &mctx.user_slot)
            } else if let Some(sw_slot) = &ctx.switch_slot {
                mon_idx_for_active_slot(state, sw_slot)
            } else {
                None
            };
            if let (Some(setter), Some(_t)) = (state.terrain_setter_mon_idx, terrain.as_ref())
                && matches!(&state.terrain_turns, Some(Unknown::Possibly(_))) {
                    let extender = terrain_extension_item(&Terrain::ElectricTerrain);
                    if ctx.config.legal_item_ok(&extender) {
                        state.predicates.push(vec![
                            Statement::HasItem { mon_idx: setter, item: extender.clone() },
                            Statement::TerrainTurns { turns: 5 },
                        ]);
                        state.predicates.push(vec![
                            Statement::Not(Box::new(Statement::HasItem {
                                mon_idx: setter,
                                item: extender,
                            })),
                            Statement::TerrainTurns { turns: 8 },
                        ]);
                    }
                }
        }
        EventKind::PseudoWeatherStart { effect } => {
            if !state.pseudo_weathers.contains(effect) {
                state.pseudo_weathers.push(effect.clone());
                state
                    .pseudo_weather_turns
                    .push(pseudo_weather_timer(effect));
            }
        }
        EventKind::PseudoWeatherEnd { effect } => {
            if let Some(pos) = state.pseudo_weathers.iter().position(|e| e == effect) {
                state.pseudo_weathers.remove(pos);
                state.pseudo_weather_turns.remove(pos);
            }
        }
        EventKind::SideConditionStart { side, condition } => {
            // Determine the setter mon_idx for I-A screen reveals.
            let setter_idx = if let Some(mctx) = &ctx.move_context {
                // Screens are only set by moves; move_context is always available here.
                mon_idx_for_active_slot(state, &mctx.user_slot)
            } else {
                None
            };
            let (conditions, turns, setters) = match side {
                Player::P1 => (
                    &mut state.p1_side_conditions,
                    &mut state.p1_side_condition_turns,
                    &mut state.p1_side_condition_setters,
                ),
                Player::P2 => (
                    &mut state.p2_side_conditions,
                    &mut state.p2_side_condition_turns,
                    &mut state.p2_side_condition_setters,
                ),
            };
            if !conditions.contains(condition) {
                conditions.push(condition.clone());
                turns.push(side_condition_timer(condition));
                setters.push(setter_idx);
                // Turn-count CNF pair for screens (Light Clay ↔ duration).
                if let (Some(setter), Some(clay), true) = (
                    setter_idx,
                    screen_extension_item(condition),
                    matches!(turns.last(), Some(Unknown::Possibly(_))),
                )
                    && ctx.config.legal_item_ok(&clay) {
                        state.predicates.push(vec![
                            Statement::HasItem { mon_idx: setter, item: clay.clone() },
                            Statement::SideConditionTurns {
                                side: *side,
                                side_condition: condition.clone(),
                                turns: 5,
                            },
                        ]);
                        state.predicates.push(vec![
                            Statement::Not(Box::new(Statement::HasItem {
                                mon_idx: setter,
                                item: clay,
                            })),
                            Statement::SideConditionTurns {
                                side: *side,
                                side_condition: condition.clone(),
                                turns: 8,
                            },
                        ]);
                    }
            }
        }
        EventKind::SideConditionEnd { side, condition } => {
            // Drop pending turn-count clauses for the ending condition (Defog / Brick
            // Break / natural expiry — the timer they refer to is going away).
            state.predicates.retain(|c| {
                !c.iter().any(|l| matches!(l,
                    Statement::SideConditionTurns { side: s, side_condition: sc, .. }
                    if s == side && sc == condition))
            });
            let (conditions, turns, setters) = match side {
                Player::P1 => (
                    &mut state.p1_side_conditions,
                    &mut state.p1_side_condition_turns,
                    &mut state.p1_side_condition_setters,
                ),
                Player::P2 => (
                    &mut state.p2_side_conditions,
                    &mut state.p2_side_condition_turns,
                    &mut state.p2_side_condition_setters,
                ),
            };
            if let Some(pos) = conditions.iter().position(|c| c == condition) {
                conditions.remove(pos);
                turns.remove(pos);
                if pos < setters.len() {
                    setters.remove(pos);
                }
            }
        }
        EventKind::SlotConditionStart { slot, condition } => {
            let slot_conds = match slot.player {
                Player::P1 => &mut state.p1_slot_conditions,
                Player::P2 => &mut state.p2_slot_conditions,
            };
            let i = slot.slot_index as usize;
            if let Some(sc_vec) = slot_conds.get_mut(i)
                && !sc_vec.contains(condition) {
                    sc_vec.push(condition.clone());
                }
        }
        EventKind::SlotConditionEnd { slot, condition } => {
            let slot_conds = match slot.player {
                Player::P1 => &mut state.p1_slot_conditions,
                Player::P2 => &mut state.p2_slot_conditions,
            };
            let i = slot.slot_index as usize;
            if let Some(sc_vec) = slot_conds.get_mut(i) {
                // Match by variant rather than full equality — conditions carry mutable data
                // (timers, heal amounts, snapshots) that differ between Start and End events.
                // For FutureMove, additionally gate on move_name to distinguish concurrent
                // Future Sight / Doom Desire queued on the same slot.
                sc_vec.retain(|c| match (c, condition) {
                    (SlotCondition::FutureMove { move_name: a, .. },
                     SlotCondition::FutureMove { move_name: b, .. }) => a != b,
                    _ => std::mem::discriminant(c) != std::mem::discriminant(condition),
                });
            }
        }

        EventKind::VolatileStart { target, volatile } => {
            if let Some(idx) = mon_idx_for_active_slot(state, target)
                && let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    use crate::state::pokemon::VolatileStatusState;
                    let already = mon.volatiles.iter().any(|v| match v {
                        VolatileStatusState::TurnStatus(vs, _) => vs == volatile,
                        VolatileStatusState::MoveStatus(vs, _) => vs == volatile,
                        VolatileStatusState::Charging(_, _) => false,
                    });
                    if !already {
                        mon.volatiles
                            .push(VolatileStatusState::TurnStatus(volatile.clone(), 0));
                    }
                }
        }
        EventKind::VolatileEnd { target, volatile } => {
            if let Some(idx) = mon_idx_for_active_slot(state, target)
                && let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    use crate::state::pokemon::VolatileStatusState;
                    mon.volatiles.retain(|v| match v {
                        VolatileStatusState::TurnStatus(vs, _) => vs != volatile,
                        VolatileStatusState::MoveStatus(vs, _) => vs != volatile,
                        VolatileStatusState::Charging(_, _) => true,
                    });
                }
        }

        EventKind::ChargingMove { user, move_used } => {
            let mut promoted_illusion = false;
            let mut discarded_before: Unknown<Species> = Unknown::Not(Vec::new());
            if let Some(idx) = mon_idx_for_active_slot(state, user)
                && let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    // Captured BEFORE mirroring can overwrite `possible_species` on a
                    // `Promoted` outcome — see `finish_illusion_promotion_restore`.
                    discarded_before = mon.possible_species.clone();
                    let learnset_dex = &ctx.config.learnset_dex;
                    let outcome = apply_with_illusion_mirroring(mon, |m| {
                        reveal_move_on_mon(m, move_used);
                        check_move_legal_for_species(m, move_used, learnset_dex);
                    });
                    promoted_illusion = matches!(outcome, IllusionMirrorOutcome::Promoted);
                    narrow_species_by_learnset(
                        mon, move_used, &ctx.config.learnset_dex, ctx.dex,
                    );
                }
            if promoted_illusion {
                resolve_zoroark_globally(state, user.player);
                finish_illusion_promotion_restore(state, user.player, discarded_before, ctx.dex, ctx.config);
            }
        }

        // ── Anticipation: add KnowsThreateningMove clause for opposing active mons ──
        // Fires when Anticipation shudders on switch-in: at least one opposing active
        // mon knows a SE or OHKO move against the holder.
        //
        // P1 is always the observer, so only a P1 holder yields useful inference — a
        // P2 shudder would constrain P1's own (already-known) mons.
        EventKind::AnticipationShudder { slot } => {
            if slot.player != Player::P1 {
                return;
            }
            // Determine the holder's defensive types.
            let holder_types: Vec<PokemonType> = if let Some(idx) =
                mon_idx_for_active_slot(state, slot)
            {
                if let Some(mon) = get_mon_by_idx(state, idx) {
                    match &mon.possible_types {
                        Unknown::Known(types) => types.clone(),
                        _ => return, // Can't form a useful clause without known types.
                    }
                } else {
                    return;
                }
            } else {
                return;
            };

            // Collect the opposing side's active mon_idxs.
            let opp_player = match slot.player {
                Player::P1 => Player::P2,
                Player::P2 => Player::P1,
            };
            let clause: Vec<Statement> = match opp_player {
                Player::P1 => (0..state.p1_active_mons.len())
                    .map(|i| Statement::KnowsThreateningMove {
                        mon_idx: i,
                        defender_types: holder_types.clone(),
                    })
                    .collect(),
                Player::P2 => {
                    let start = p2_mon_start(state);
                    (0..state.p2_active_mons.len())
                        .map(|i| Statement::KnowsThreateningMove {
                            mon_idx: start + i,
                            defender_types: holder_types.clone(),
                        })
                        .collect()
                }
            };
            if !clause.is_empty() {
                state.predicates.push(clause);
            }
        }

        // ── IllusionEnded: reveal the true species when the disguise breaks ────────
        // This is an AUTHORITATIVE resolution (direct-damage break, or ability
        // suppression/change — never fires for indirect damage): the slot's PRIMARY
        // (shown-species) identity was never real, and `possible_illusion_state` (if
        // still live) is confirmed correct. Promote it wholesale rather than
        // rebuilding species/types/ability/stats from scratch — the hypothesis has
        // been independently tracked and narrowed by every mirrored pass since it was
        // seeded, so it already carries whatever this physical mon's own history has
        // taught us (revealed moves, item, etc.), which a from-scratch rebuild would
        // throw away.
        EventKind::IllusionEnded { slot, actual_species } => {
            if let Some(idx) = mon_idx_for_active_slot(state, slot) {
                let discarded_species = get_mon_by_idx(state, idx)
                    .and_then(|m| match &m.possible_species {
                        Unknown::Known(s) => Some(s.clone()),
                        _ => None,
                    });
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    if let Some(sub) = mon.possible_illusion_state.take() {
                        promote_illusion_to_primary(mon, *sub);
                    } else {
                        // Defensive fallback (shouldn't happen in normal play — every
                        // eligible mon is seeded with a hypothesis at battle start):
                        // overwrite directly and recompute worst/best-case bounds for
                        // the revealed species, same as a brand-new sighting would get.
                        mon.possible_species = Unknown::Known(actual_species.clone());
                        if let Some(data) = ctx.dex.get(actual_species) {
                            mon.possible_types = Unknown::Known(data.types.clone());
                            let new_abilities = if data.abilities.is_empty() {
                                Unknown::Not(Vec::new())
                            } else {
                                Unknown::Possibly(data.abilities.clone())
                            };
                            mon.possible_original_abilities = new_abilities.clone();
                            mon.possible_abilities = new_abilities;
                            mon.possible_weight_hg = Unknown::Known(data.weight);
                        }
                        // See the historical note below on why stat bounds must be
                        // reset rather than left as the disguise species' window.
                        recompute_stats_for_iv_mode(mon, actual_species, ctx.dex, ctx.config);
                        mon.min_evs = [0; 6];
                        mon.max_evs = [252; 6];
                    }
                }
                // Historical note (kept for context): stat/EV bounds narrowed by Pass
                // 3/5 while this slot displayed the disguise were back-solved against
                // the DISGUISE species' base stat table — a small-base-HP mon (e.g.
                // Zoroark, base 60) revealed from behind a large-base-HP disguise
                // (e.g. Snorlax, base 160) could leave min/max HP pinned to a window no
                // Zoroark IV/EV combination can reach. Promotion sidesteps this
                // entirely: the hypothesis being promoted was ALWAYS tracked against
                // Zoroark's own base stats (mirrored independently since it was
                // seeded), never the disguise's — there is no stale disguise-derived
                // window to reset in the promoted case, only in the defensive fallback
                // above (which mirrors what `pass1_switch` gives a brand-new sighting).

                // Persisted CNF clauses referencing this slot that were derived from
                // the DISGUISE species' base stats/movepool (EVIVStatGE/LE,
                // SpeedComparison, NatureBoostsStat/NerfsStat, KnowsThreateningMove)
                // are now stale for the promoted identity, for the same reason
                // `Transformed` purges them below — left in place, BCP could re-tighten
                // bounds right back to a disguise-derived value on the next fixpoint.
                state
                    .predicates
                    .retain(|clause| !clause.iter().any(|lit| statement_stale_after_species_reveal(lit, idx)));

                // The side's Illusion forme is now positively located: drop every
                // remaining sibling hypothesis once fully accounted for.
                resolve_zoroark_globally(state, slot.player);

                // The discarded PRIMARY identity (e.g. "Snorlax") was never actually
                // confirmed on the field at all — the active slot was secretly this
                // side's Illusion forme the whole time. The real Snorlax must still be
                // unaccounted for somewhere in the party; restore it to `possible_back`
                // at a fresh, unrevealed baseline (everything "observed" about it while
                // it looked active was actually the Illusion forme's behavior, so none
                // of it is trustworthy information about the real mon). Skip if a
                // matching entry already exists in the bench (e.g. the doubles
                // two-in-front case never consumed one in the first place — see
                // `maybe_resolve_illusion_two_in_front` — so restoring here would
                // create a phantom duplicate).
                if let Some(discarded) = discarded_species
                    && !unknowns::is_illusion_capable_species(&discarded)
                    && !combined_back(state, &slot.player)
                        .iter()
                        .any(|m| unknown_is_known_as(&m.possible_species, &discarded))
                {
                    restore_discarded_primary_to_bench(state, slot.player, discarded, ctx.dex, ctx.config);
                }

                // The Illusion forme's OWN benched baseline entry (species known,
                // sitting untouched in `known_back`/`possible_back` this whole time —
                // see `seed_illusion_hypotheses`) is now a stale duplicate of this same
                // physical Pokémon. Left in place, `teammate_indices`/`enforce_unique_item`
                // would see two teammates holding the same resolved item. Discard it.
                let known_back = match slot.player {
                    Player::P1 => &mut state.p1_known_back_mons,
                    Player::P2 => &mut state.p2_known_back_mons,
                };
                if let Some(pos) = known_back
                    .iter()
                    .position(|m| unknown_is_known_as(&m.possible_species, actual_species))
                {
                    known_back.remove(pos);
                } else {
                    let possible_back = match slot.player {
                        Player::P1 => &mut state.p1_possible_back_mons,
                        Player::P2 => &mut state.p2_possible_back_mons,
                    };
                    if let Some(pos) = possible_back
                        .iter()
                        .position(|m| unknown_is_known_as(&m.possible_species, actual_species))
                    {
                        possible_back.remove(pos);
                    }
                }
            }
        }

        // ── Transformed: overlay the copy source's fog identity (S26) ─────────────
        // The transformer adopts the copy source's species/types/stats/ability/moves/
        // boosts; its own HP, max HP, item, status, level, nature, EVs, IVs are kept.
        // We read the FOG entry at `into_slot`, so copying our own Known mon yields
        // exact stats and copying a hidden opponent inherits that opponent's bounds.
        EventKind::Transformed { slot, into_slot, into_species } => {
            let src_idx = mon_idx_for_active_slot(state, into_slot);
            let dst_idx = mon_idx_for_active_slot(state, slot);
            if let (Some(si), Some(di)) = (src_idx, dst_idx)
                && let Some(src) = get_mon_by_idx(state, si).cloned()
                    && let Some(mon) = get_mon_mut_by_idx(state, di) {
                        // Save the pre-transform snapshot once (revert on switch-out).
                        if mon.pre_transform.is_none() {
                            mon.pre_transform = Some(Box::new(mon.clone()));
                        }
                        // Displayed species is authoritative (matches what the player
                        // sees); everything else is copied from the source's fog view.
                        mon.possible_species = Unknown::Known(into_species.clone());
                        mon.possible_types = src.possible_types.clone();
                        mon.possible_weight_hg = src.possible_weight_hg.clone();
                        mon.possible_genders = src.possible_genders.clone();
                        mon.possible_abilities = src.possible_abilities.clone();
                        mon.possible_original_abilities = src.possible_original_abilities.clone();
                        mon.boosts = src.boosts;
                        // Moves copied; PP (and max PP) capped at 5 per Transform rules.
                        mon.known_moves = src.known_moves.clone();
                        for i in 0..4 {
                            if src.max_pp[i] >= 0 {
                                let capped = src.max_pp[i].min(5);
                                mon.max_pp[i] = capped;
                                mon.move_pp[i] = capped;
                            } else {
                                mon.max_pp[i] = -1;
                                mon.move_pp[i] = -1;
                            }
                        }
                        // Stats and their EV/IV/nature back-solve inputs come from the
                        // source for the five non-HP stats; HP (index 0) is NOT copied.
                        for i in 1..6 {
                            mon.min_stats[i] = src.min_stats[i];
                            mon.max_stats[i] = src.max_stats[i];
                            mon.min_pre_nature_stat[i] = src.min_pre_nature_stat[i];
                            mon.max_pre_nature_stat[i] = src.max_pre_nature_stat[i];
                        }
                    }
            // Pre-transform SpeedComparison / EVIV / HasAbility clauses describe the
            // transformer's OWN pre-copy stats and ability — stale now. Drop them
            // (predicate purge only; setter records are unaffected by a transform).
            if let Some(di) = dst_idx {
                state
                    .predicates
                    .retain(|clause| !clause.iter().any(|lit| statement_references_mon(lit, di)));
            }
        }

        // Events with no direct state update in Pass 1 — all handled by
        // enclosing MoveUsed context in Passes 2/3 or by reactions.
        // When a Pokémon fails to move due to flinch, attempt to attribute the flinch
        // cause (King's Rock / Razor Fang / Stench) to the opposing attacker.
        // VolatileStart{Flinch} is suppressed in the simulator so this is the only
        // game-legal point where flinch becomes observable to the opponent.
        EventKind::Cant {
            reason: CantReason::Flinch,
            slot,
        } => {
            pass2_flinch_holder_from_cant(state, slot, ctx);
        }

        EventKind::Crit { .. }
        | EventKind::Immune { .. }
        | EventKind::Missed { .. }
        | EventKind::MoveFailed { .. }
        | EventKind::Blocked { .. }
        | EventKind::HitCount { .. }
        | EventKind::Cant { .. }
        | EventKind::MustRecharge { .. }
        | EventKind::SingleMoveOrTurn { .. }
        | EventKind::PerishCount { .. } => {}
    }
}

// ── Pass 1 helpers ────────────────────────────────────────────────────────────

fn pass1_apply_switch_event(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    ctx: &BattleContext,
) {
    match &event.kind {
        EventKind::Switch(sw) => {
            // S18: the active mon_idx identifies a SLOT, not a Pokémon — drop every
            // persisted clause/setter record about the outgoing occupant before the
            // slot is re-bound, or they silently constrain the incoming mon.
            purge_mon_scoped_knowledge(state, &sw.slot);
            apply_switch_out_reset(state, &sw.slot);
            // B1: preserve the outgoing mon to the bench so its state (HP, move reveals,
            // ability/item narrowing) survives for future re-entry inference.
            bench_outgoing_mon(state, &sw.slot, &sw.species);
            pass1_switch(state, sw, ctx);
            pass1_ability_absence_inference(state, &[sw.slot], &event.reactions, ctx);
        }
        EventKind::SimultaneousSwitch { switches } => {
            // The concrete simulator emits `switches` in GLOBAL cross-side effective-
            // speed order (`process_sendouts_in_speed_order_branching`,
            // simulator/mod.rs), NOT slot-index order — a side's own faster lead can
            // appear before its slower teammate even though it occupies a HIGHER
            // slot_index. `pass1_switch`'s active-slot placement
            // (`if slot_i < actives.len() {overwrite} else {push}`) silently assumes
            // ascending slot-index processing per side: processing the higher slot
            // first pushes it to the WRONG array position (since the Vec is still
            // short), and the lower slot processed next then overwrites that
            // position — silently destroying the first mon with no bench record at
            // all. This was the root cause of a live BCP soundness panic (a stale
            // `SpeedComparison` baked in against the wrong, shifted `mon_idx` once
            // the roster later grew back to its correct length). Sort a per-event
            // copy by slot_index so both placement-sensitive loops below always see
            // ascending order, regardless of the event's real (speed-encoded) order.
            let mut switches_by_slot: Vec<&SwitchState> = switches.iter().collect();
            switches_by_slot.sort_by_key(|sw| sw.slot.slot_index);

            for sw in &switches_by_slot {
                // S18: see the single-switch arm above.
                purge_mon_scoped_knowledge(state, &sw.slot);
                apply_switch_out_reset(state, &sw.slot);
                // B1: preserve each outgoing mon before any pass1_switch replaces its slot.
                bench_outgoing_mon(state, &sw.slot, &sw.species);
            }
            for sw in &switches_by_slot {
                pass1_switch(state, sw, ctx);
            }
            // Ability-absence inference cares about the REAL activation order (e.g.
            // Unnerve vs Sand Stream) — keep the original (speed-encoded) event
            // order here, unlike the two placement loops above.
            let slots: Vec<FieldSlot> = switches.iter().map(|sw| sw.slot).collect();
            pass1_ability_absence_inference(state, &slots, &event.reactions, ctx);
        }
        _ => {}
    }
}

/// `true` if `stmt` (recursing through `Not`) constrains the Pokémon at `mon_idx`.
fn statement_references_mon(stmt: &Statement, idx: usize) -> bool {
    match stmt {
        Statement::Not(inner) => statement_references_mon(inner, idx),
        Statement::HasItem { mon_idx, .. }
        | Statement::HasAbility { mon_idx, .. }
        | Statement::NatureBoostsStat { mon_idx, .. }
        | Statement::NatureNerfsStat { mon_idx, .. }
        | Statement::EVIVStatGE { mon_idx, .. }
        | Statement::EVIVStatLE { mon_idx, .. }
        | Statement::KnowsThreateningMove { mon_idx, .. } => *mon_idx == idx,
        Statement::SpeedComparison { fast_idx, slow_idx, .. } => {
            *fast_idx == idx || *slow_idx == idx
        }
        Statement::WeatherTurns { .. }
        | Statement::TerrainTurns { .. }
        | Statement::SideConditionTurns { .. } => false,
    }
}

/// `true` if `lit` references `idx` (see `statement_references_mon`) AND is a
/// species-VALUE statement — one whose numeric/relational content (a stat bound, a
/// speed comparison, a nature-stat direction, a movepool-dependent threat check) was
/// derived from a specific species' base-stat table or movepool. Used by
/// `IllusionEnded`'s predicate purge (S30) to distinguish clauses invalidated by a
/// species reveal from `HasSpecies`/`HasItem`/`HasAbility` identity-tie clauses, which
/// stay valid (and in fact become *resolvable*) once the true species is Known —
/// purging those too would discard the very mechanism that ties the disguise's item to
/// its true identity.
fn statement_stale_after_species_reveal(lit: &Statement, idx: usize) -> bool {
    if !statement_references_mon(lit, idx) {
        return false;
    }
    let inner = match lit {
        Statement::Not(inner) => inner.as_ref(),
        other => other,
    };
    matches!(
        inner,
        Statement::EVIVStatGE { .. }
            | Statement::EVIVStatLE { .. }
            | Statement::SpeedComparison { .. }
            | Statement::NatureBoostsStat { .. }
            | Statement::NatureNerfsStat { .. }
            | Statement::KnowsThreateningMove { .. }
    )
}

/// S18: an active `mon_idx` is a *slot* index — stable as a number (see S1), but
/// re-bound to a different physical Pokémon on every switch. Persisted `Statement`s
/// (SpeedComparison, HasItem/HasAbility disjunctions, EVIV bounds) and the weather/
/// terrain/screen setter records store the slot index of the mon they were observed
/// on; left in place, BCP and the timer machinery would force them against the
/// incoming Pokémon (e.g. a SpeedComparison from Mon A's move order raising fresh
/// switch-in B's min Spe, or a timer collapse revealing A's Heat Rock as Known on B).
///
/// Called when the occupant leaves. Dropping a clause referencing the slot only
/// widens (sound); the cost is completeness — predicate-only knowledge about the
/// benched mon is forgotten. Field-level knowledge (item/ability/EV bounds) survives
/// via `bench_outgoing_mon`.
fn purge_mon_scoped_knowledge(state: &mut UnknownBattleState, slot: &FieldSlot) {
    let Some(idx) = mon_idx_for_active_slot(state, slot) else {
        return; // Initial lead send-out: the slot was never occupied.
    };
    state
        .predicates
        .retain(|clause| !clause.iter().any(|lit| statement_references_mon(lit, idx)));
    if state.weather_setter_mon_idx == Some(idx) {
        state.weather_setter_mon_idx = None;
    }
    if state.terrain_setter_mon_idx == Some(idx) {
        state.terrain_setter_mon_idx = None;
    }
    for setter in state
        .p1_side_condition_setters
        .iter_mut()
        .chain(state.p2_side_condition_setters.iter_mut())
    {
        if *setter == Some(idx) {
            *setter = None;
        }
    }
}

/// `true` if `stmt` (recursing through `Not`) is a `HasItem` literal about `mon_idx`.
fn statement_is_item_literal_for(stmt: &Statement, idx: usize) -> bool {
    match stmt {
        Statement::Not(inner) => statement_is_item_literal_for(inner, idx),
        Statement::HasItem { mon_idx, .. } => *mon_idx == idx,
        _ => false,
    }
}

/// Truth value of an item literal about `mon_idx` *in the holding window that just
/// ended*, given the mon held `outgoing` throughout it. `None` = not an item literal
/// about this mon (leave untouched).
fn historical_item_literal_value(stmt: &Statement, idx: usize, outgoing: &Item) -> Option<bool> {
    match stmt {
        Statement::Not(inner) => historical_item_literal_value(inner, idx, outgoing).map(|b| !b),
        Statement::HasItem { mon_idx, item } if *mon_idx == idx => Some(item == outgoing),
        _ => None,
    }
}

/// S19: `HasItem` clauses encode "held item X *at observation time*", but BCP
/// evaluates them against the mon's *current* item. Once the held item changes
/// (Knock Off/Thief/Fling → `ItemLost`; consumption → `ItemLost{consumed}`;
/// Trick/Switcheroo/Recycle/Pickup → `ItemGained`), every persisted clause about this
/// mon's item is stale: `[HasItem(BrightPowder) ∨ HasItem(LaxIncense)]` became an
/// unsatisfiable-clause panic once the explaining item was knocked off (item now
/// `Known(None)` falsifies both literals), and `[EVIVStatGE ∨ HasItem(OccaBerry)]`
/// was unit-forced once the berry disjunct was falsified by its own consumption —
/// raising the stat floor above the true berry-world value.
///
/// Every surviving clause was emitted (or last resolved) during the holding window
/// that just ended; running this at every item-change event maintains that invariant
/// inductively. With the outgoing item known, each item literal about this mon has a
/// definite historical truth value: satisfied clauses are dropped, falsified literals
/// pruned (a pruned-to-unit clause
/// is forced by the next BCP run, e.g. the `[HasItem(DampRock) ∨ WeatherTurns{5}]`
/// pair correctly collapses to the base-duration branch when the setter turns out to
/// have held a berry). When the outgoing item is unknown (`ItemGained` onto an
/// unresolved item), the clauses cannot be resolved and are purged — sound, since
/// removing a constraint only widens.
fn resolve_item_clauses_on_item_change(
    state: &mut UnknownBattleState,
    idx: usize,
    outgoing: Option<Item>,
) {
    let Some(outgoing) = outgoing else {
        state
            .predicates
            .retain(|clause| !clause.iter().any(|lit| statement_is_item_literal_for(lit, idx)));
        return;
    };
    let mut i = 0;
    while i < state.predicates.len() {
        let clause = &state.predicates[i];
        // A historically-true literal satisfies the whole (disjunctive) clause.
        if clause
            .iter()
            .any(|lit| historical_item_literal_value(lit, idx, &outgoing) == Some(true))
        {
            state.predicates.remove(i);
            continue;
        }
        let pruned: Vec<Statement> = clause
            .iter()
            .filter(|lit| historical_item_literal_value(lit, idx, &outgoing) != Some(false))
            .cloned()
            .collect();
        if pruned.is_empty() {
            inference_contradiction!(
                idx,
                "clause has no explanation left after resolving item literals against \
                 the outgoing item {:?}: {:?}\nmon_idx legend: {}",
                outgoing,
                clause,
                mon_idx_legend(state)
            );
        }
        if pruned.len() != state.predicates[i].len() {
            state.predicates[i] = pruned;
        }
        i += 1;
    }
}

/// Push the mon currently at `slot` (if any and not fainted) onto the appropriate
/// bench list with its post-`apply_switch_out_reset` state (HP intact, boosts/volatiles
/// cleared).  Called immediately after `apply_switch_out_reset` and before `pass1_switch`
/// overwrites the active slot, so the bench baseline reflects the last-seen HP.
///
/// Mons with `Unknown::Known` species go to the `known_back_mons` list so that
/// `pass1_switch` can find them by species on re-entry.  Unknown-species mons go to
/// `possible_back_mons`.
///
/// S37 companion (generalized — was hardcoded to P1's side, fixed alongside
/// `pass1_switch`'s own S37 guard): no-ops when `incoming_species` already
/// matches the slot's current occupant on WHICHEVER side is the viewer's own —
/// the one-time team-preview→battle transition where `into_battle_state`
/// pre-places the viewer's own lead directly into that side's `*_active_mons`
/// and the battle-phase lead-reveal `SimultaneousSwitch` then re-announces that
/// same mon. `pass1_switch`'s S37 guard (below) detects this and returns early,
/// keeping the existing active entry rather than consuming a bench match — so if
/// this function benched the mon anyway, the clone it just pushed would never be
/// removed, leaving the same physical Pokémon duplicated across `*_active_mons`
/// and `*_known_back_mons` (seen in practice as a false item-clause
/// contradiction: the duplicate collides with its own already-`Known` item).
/// Since this function is hardcoded per-player, the two guards must agree on
/// which side is "already placed" for the SAME transition — hardcoding both to
/// P1 only worked for a P1 belief; for a P2 belief it's P2's side that's
/// pre-placed, and leaving this one P1-only would silently re-introduce the
/// orphaned-duplicate bug on P2's side instead. A genuine mid-battle switch
/// always changes who's active, so this condition can only fire at the one-time
/// reveal, regardless of player — no real switch-out is ever skipped. Mirrors
/// `purge_mon_scoped_knowledge`'s own "nothing actually left" self-guard just
/// above.
fn bench_outgoing_mon(state: &mut UnknownBattleState, slot: &FieldSlot, incoming_species: &Species) {
    let slot_i = slot.slot_index as usize;
    let already_placed = match slot.player {
        Player::P1 => state.p1_active_mons.get(slot_i),
        Player::P2 => state.p2_active_mons.get(slot_i),
    }
    .is_some_and(|m| unknown_is_known_as(&m.possible_species, incoming_species));
    if already_placed {
        return;
    }
    // Clone in a temporary scope to avoid simultaneous mutable borrows.
    let maybe_benched: Option<UnknownPokemonState> = {
        let actives = match slot.player {
            Player::P1 => &state.p1_active_mons,
            Player::P2 => &state.p2_active_mons,
        };
        actives.get(slot_i).cloned()
    };
    if let Some(benched) = maybe_benched {
        if benched.fainted {
            // Route to the fainted bucket rather than dropping it: it's outside
            // the `mon_idx` flat-index space and excluded from `combined_back`
            // (see the field doc comment on `UnknownBattleState::p1/p2_fainted_mons`).
            // This preserves the knowledge accumulated about the mon (species,
            // revealed moves/item/ability) for display instead of silently
            // discarding it, which previously made fainted-then-replaced opponent
            // mons vanish from both the "back" and "fainted" UI sections.
            match slot.player {
                Player::P1 => state.p1_fainted_mons.push(benched),
                Player::P2 => state.p2_fainted_mons.push(benched),
            }
        } else {
            // `possible_species` is always `Known` under the parallel-hypothesis
            // Zoroark model (a mon's shown identity is never itself ambiguous —
            // Zoroark ambiguity lives entirely in `possible_illusion_state`, which
            // moves along for free since it's a nested field on `benched` and this
            // whole value is being moved, not rebuilt). Previously (S29) an
            // unresolved disguise widened `possible_species` itself to a `Possibly`
            // that this branch discarded rather than benched, to avoid double-
            // counting a physical roster member; that widening no longer happens,
            // so every non-fainted leaver benches uniformly here, hypothesis intact
            // — this is exactly the "switch-out persists, doesn't discard" behavior
            // the Zoroark lifecycle depends on (see `possible_illusion_state`'s doc
            // comment).
            match slot.player {
                Player::P1 => state.p1_known_back_mons.push(benched),
                Player::P2 => state.p2_known_back_mons.push(benched),
            }
        }
    }
}

/// Every benched entry for `player`'s side — `known_back` AND `possible_back`
/// combined. A bench scan for "is there a Zoroark somewhere on this team" must
/// cover both: species is `Known` on `possible_back` entries too (team preview
/// reveals species for the whole roster — see `from_opponent_species`/
/// `into_battle_state`; only item/ability/nature/EVs stay genuinely unresolved
/// there). Scanning `known_back` alone would miss any bench mon that hasn't yet
/// been individually revealed-and-switched-out, which after the fix making
/// `into_battle_state` populate `possible_back` from turn 1 (TODO.md: back mons
/// must not "immediately show up") is now most of the roster for the whole early
/// game.
fn combined_back<'a>(state: &'a UnknownBattleState, player: &Player) -> Vec<&'a UnknownPokemonState> {
    match player {
        Player::P1 => {
            state.p1_known_back_mons.iter().chain(state.p1_possible_back_mons.iter()).collect()
        }
        Player::P2 => {
            state.p2_known_back_mons.iter().chain(state.p2_possible_back_mons.iter()).collect()
        }
    }
}

fn pass1_switch(state: &mut UnknownBattleState, sw: &SwitchState, ctx: &BattleContext) {
    let player = &sw.slot.player;
    let slot_i = sw.slot.slot_index as usize;
    let species = &sw.species;

    // S37 (generalized — was hardcoded to Player::P1, fixed alongside the
    // SimultaneousSwitch ordering bug above): the team-preview→battle transition
    // (`into_battle_state`) already places the VIEWER's OWN leads directly into
    // that side's `*_active_mons` with their full `Known` nature/EV/IV data from
    // `from_known_pokemon` — that side is never actually "found in the back and
    // moved to active" the way this function's normal (opponent, or a genuine
    // mid-battle switch) logic assumes. Whichever side that is (P1's belief pre-
    // places P1; P2's belief pre-places P2 — `pass1_switch` has no "viewer" of
    // its own to check, so detect it structurally instead), a lead's
    // SimultaneousSwitch reveal at battle start never matches anything in that
    // side's known_back/possible_back, and this function fell through to its
    // "completely new mon" branch — REBUILDING the viewer's own, fully-known lead
    // as a wide-uncertain `from_opponent_species` mon (nature `Not([])`, EVs
    // `[0,252]`) and overwriting the correct entry `into_battle_state` had
    // already placed. A genuine mid-battle switch always changes who's active,
    // so the incoming species can never already match the slot's CURRENT
    // occupant except at this one-time reveal — regardless of which player it
    // is — so detect that case and just apply the transient switch fields
    // (HP/status/etc.) to the existing, already-correct entry instead of
    // discarding it.
    let already_placed = match player {
        Player::P1 => state.p1_active_mons.get(slot_i),
        Player::P2 => state.p2_active_mons.get(slot_i),
    }
    .is_some_and(|existing| unknown_is_known_as(&existing.possible_species, species));
    if already_placed {
        let actives = match player {
            Player::P1 => &mut state.p1_active_mons,
            Player::P2 => &mut state.p2_active_mons,
        };
        if let Some(mon) = actives.get_mut(slot_i) {
            apply_switch_state_to_mon(mon, sw, ctx.config);
        }
        return;
    }

    // Under the parallel-hypothesis Zoroark model, `possible_species` is always
    // pinned (never a `Possibly` disjunction) — a mon's Zoroark ambiguity lives
    // entirely in its own `possible_illusion_state`, which was already seeded
    // once, upfront, on every eligible roster entry at the team-preview→battle
    // transition (`seed_illusion_hypotheses` in `unknowns.rs`). That means
    // straightforward species-matching against the bench is now ALWAYS sound —
    // no S29 special-case needed: whichever physical roster member's true
    // species matches the shown species is exactly the one being pulled onto
    // the field, hypothesis (if any) riding along automatically since the whole
    // `UnknownPokemonState` is moved, not rebuilt.
    let known = match player {
        Player::P1 => &mut state.p1_known_back_mons,
        Player::P2 => &mut state.p2_known_back_mons,
    };
    let back_mon: Option<UnknownPokemonState> = if let Some(pos) =
        known.iter().position(|m| unknown_is_known_as(&m.possible_species, species))
    {
        Some(known.remove(pos))
    } else {
        let possible = match player {
            Player::P1 => &mut state.p1_possible_back_mons,
            Player::P2 => &mut state.p2_possible_back_mons,
        };
        possible
            .iter()
            .position(|m| unknown_is_known_as(&m.possible_species, species))
            .map(|pos| possible.remove(pos))
    };

    // Did this switch-in bring the real Illusion forme itself onto the field,
    // undisguised (it was the last conscious party member, so Illusion had no
    // one else to copy)? Per Bulbapedia, only the true Illusion forme can ever
    // be shown AS the Illusion forme's own species — no other mon can impersonate
    // it — so this positively resolves its location the instant it's seen.
    let resolves_illusion_forme =
        unknowns::is_illusion_capable_species(species) && match player {
            Player::P1 => state.p1_unresolved_zoroark_count > 0,
            Player::P2 => state.p2_unresolved_zoroark_count > 0,
        };

    let mut mon = if let Some(mut m) = back_mon {
        // B2: before apply_switch_state_to_mon overwrites m.hp, compare the benched
        // baseline against the incoming HP to infer (or exclude) Regenerator.
        infer_regenerator_from_hp_delta(&mut m, sw, state);
        m
    } else {
        // Not found on the bench — either a genuinely new opponent mon (a species
        // outside the known roster, e.g. a synthetic test), or this physical roster
        // member's bench entry was already consumed/removed by earlier bookkeeping
        // (e.g. an Illusion decoy restored via `restore_discarded_primary_to_bench`
        // and then immediately re-matched here, before F1 existed). Prefer the
        // pristine team-preview snapshot (`find_roster_template`) over rebuilding
        // species-only — under an open team sheet that snapshot still carries the
        // fully-`Known` item/moves/ability/nature set, same reasoning as
        // `restore_discarded_primary_to_bench`. In normal operation every roster
        // member was already seeded into `possible_back` at team preview (including
        // its Zoroark hypothesis, if eligible) and would have been found by the bench
        // search above, so reaching this branch at all is the defensive case.
        let mut new_mon = if let Some(template) = find_roster_template(state, *player, species) {
            let mut t = template.clone();
            t.possible_illusion_state = None; // defensive; templates never carry one
            t
        } else {
            // Genuinely new: build from species, then recompute stat bounds for the
            // configured IV mode (always call — fixes the bug where non-force_max_ivs
            // mode left the mon with the from_opponent_species defaults instead of
            // proper bounds).
            let mut new_mon =
                UnknownPokemonState::from_opponent_species(species.clone(), ctx.dex, ctx.config.level);
            recompute_stats_for_iv_mode(&mut new_mon, species, ctx.dex, ctx.config);
            if let Some(legal) = &ctx.config.legal_items {
                let mut candidates: Vec<Item> = legal.iter().cloned().collect();
                candidates.push(Item::None);
                new_mon.item = Unknown::Possibly(candidates);
            }
            new_mon
        };
        maybe_seed_fresh_hypothesis(state, *player, &mut new_mon);
        new_mon
    };

    apply_switch_state_to_mon(&mut mon, sw, ctx.config);

    let actives = match sw.slot.player {
        Player::P1 => &mut state.p1_active_mons,
        Player::P2 => &mut state.p2_active_mons,
    };
    if slot_i < actives.len() {
        actives[slot_i] = mon;
    } else {
        actives.push(mon);
    }

    if resolves_illusion_forme {
        resolve_zoroark_globally(state, *player);
    } else {
        maybe_resolve_illusion_two_in_front(state, &sw.slot, species, ctx);
    }
}

/// Doubles-only refinement: if the SAME species is now shown on two active
/// slots on the same side simultaneously, Species Clause guarantees only one
/// of them is the real thing — the other must be this side's Illusion forme in
/// disguise (no other mon can impersonate anything). This is a genuine
/// exclusive-or between the two slots, but rather than encode that correlation
/// precisely (which would need a dedicated cross-mon-idx CNF clause), this
/// attaches an independent hypothesis to the JUST-ARRIVED slot from the side's
/// current baseline — sound (never excludes the true possibility) even though
/// it doesn't capture "resolving one slot pins the other," which is a known,
/// documented precision gap, not a soundness one.
fn maybe_resolve_illusion_two_in_front(
    state: &mut UnknownBattleState,
    slot: &FieldSlot,
    species: &Species,
    ctx: &BattleContext,
) {
    if state.active_per_side < 2 {
        return;
    }
    let unresolved = match slot.player {
        Player::P1 => state.p1_unresolved_zoroark_count,
        Player::P2 => state.p2_unresolved_zoroark_count,
    };
    if unresolved == 0 {
        return;
    }
    let actives = match slot.player {
        Player::P1 => &state.p1_active_mons,
        Player::P2 => &state.p2_active_mons,
    };
    let slot_i = slot.slot_index as usize;
    let duplicate_elsewhere = actives.iter().enumerate().any(|(i, m)| {
        i != slot_i && unknown_is_known_as(&m.possible_species, species)
    });
    if !duplicate_elsewhere {
        return;
    }
    // Find the side's Illusion-forme baseline (still sitting in the bench,
    // unresolved) to seed a hypothesis for the newly-arrived slot, if it
    // doesn't already carry one (e.g. a returning mon resuming its own).
    let baseline = combined_back(state, &slot.player)
        .into_iter()
        .find(|m| {
            matches!(&m.possible_species, Unknown::Known(s) if unknowns::is_illusion_capable_species(s))
        })
        .cloned();
    let Some(baseline) = baseline else { return };
    let actives_mut = match slot.player {
        Player::P1 => &mut state.p1_active_mons,
        Player::P2 => &mut state.p2_active_mons,
    };
    if let Some(mon) = actives_mut.get_mut(slot_i)
        && mon.possible_illusion_state.is_none()
    {
        mon.possible_illusion_state =
            Some(Box::new(unknowns::seed_illusion_hypothesis_for(mon, &baseline)));
    }
    let _ = ctx; // reserved for future dex-dependent refinements
}

/// Recompute `min_stats`/`max_stats` and pin IVs according to `config.force_max_ivs`.
///
/// When `force_max_ivs = true`: IVs are pinned to [31;6], min stats use EV=0 and nature×0.9,
///   max stats use EV=252 and nature×1.1.
/// When `force_max_ivs = false`: IV range is [0,31], min uses IV=0 + EV=0 + nature×0.9,
///   max uses IV=31 + EV=252 + nature×1.1.
///
/// Takes `dex`/`config` directly (rather than a full `BattleContext`) so it can also be
/// called from `bcp::force_literal`, which resolves a `HasSpecies` literal outside the
/// event-walk's `BattleContext` scope (see S30).
fn recompute_stats_for_iv_mode(
    mon: &mut UnknownPokemonState,
    species: &Species,
    dex: &HashMap<Species, PokemonData>,
    config: &InferenceConfig,
) {
    let force_max = config.force_max_ivs;
    if force_max {
        mon.min_ivs = [31; 6];
        mon.max_ivs = [31; 6];
    } else {
        mon.min_ivs = [0; 6];
        mon.max_ivs = [31; 6];
    }
    if let Some(data) = dex.get(species) {
        let b = data.base_stats;
        let lv = config.level;
        let min_iv: u8 = if force_max { 31 } else { 0 };
        mon.min_stats = [
            calc_hp(b[0], min_iv, 0, lv),
            calc_stat(b[1], min_iv, 0, lv, 0.9),
            calc_stat(b[2], min_iv, 0, lv, 0.9),
            calc_stat(b[3], min_iv, 0, lv, 0.9),
            calc_stat(b[4], min_iv, 0, lv, 0.9),
            calc_stat(b[5], min_iv, 0, lv, 0.9),
        ];
        mon.max_stats = [
            calc_hp(b[0], 31, 252, lv),
            calc_stat(b[1], 31, 252, lv, 1.1),
            calc_stat(b[2], 31, 252, lv, 1.1),
            calc_stat(b[3], 31, 252, lv, 1.1),
            calc_stat(b[4], 31, 252, lv, 1.1),
            calc_stat(b[5], 31, 252, lv, 1.1),
        ];
        // Initialise pre-nature BSV bounds (neutral mod = 1.0).
        mon.min_pre_nature_stat = [
            calc_hp(b[0], min_iv, 0, lv),
            calc_stat(b[1], min_iv, 0, lv, 1.0),
            calc_stat(b[2], min_iv, 0, lv, 1.0),
            calc_stat(b[3], min_iv, 0, lv, 1.0),
            calc_stat(b[4], min_iv, 0, lv, 1.0),
            calc_stat(b[5], min_iv, 0, lv, 1.0),
        ];
        mon.max_pre_nature_stat = [
            calc_hp(b[0], 31, 252, lv),
            calc_stat(b[1], 31, 252, lv, 1.0),
            calc_stat(b[2], 31, 252, lv, 1.0),
            calc_stat(b[3], 31, 252, lv, 1.0),
            calc_stat(b[4], 31, 252, lv, 1.0),
            calc_stat(b[5], 31, 252, lv, 1.0),
        ];
    }
}

/// After a Mega Evolution / permanent Forme Change swaps in a new base-stat table for
/// an already-tracked mon, remap the stat-bound fields against the new base using the
/// mon's EXISTING (possibly already-tightened) EV/IV/nature bounds.
///
/// Mega Evolution and forme changes don't alter EVs, IVs, or nature — only the
/// base-stat table. Unlike `recompute_stats_for_iv_mode` (for a brand-new sighting),
/// this must NOT reset EV/IV/nature bounds to the theoretical worst/best case, which
/// would discard information already gained from prior observations (Pass 3/5). The
/// bug this fixes: leaving the old species' stat window in place is inconsistent with
/// the new base stats, and pass5 then sees an impossible constraint.
fn recompute_stat_bounds_for_species_change(
    mon: &mut UnknownPokemonState,
    new_base: [u16; 6],
    level: u8,
) {
    // HP: no nature modifier.
    mon.min_pre_nature_stat[0] = calc_hp(new_base[0], mon.min_ivs[0], mon.min_evs[0], level);
    mon.max_pre_nature_stat[0] = calc_hp(new_base[0], mon.max_ivs[0], mon.max_evs[0], level);
    mon.min_stats[0] = mon.min_pre_nature_stat[0];
    mon.max_stats[0] = mon.max_pre_nature_stat[0];

    const STATS: [PokemonStat; 5] = [
        PokemonStat::Atk,
        PokemonStat::Def,
        PokemonStat::SpA,
        PokemonStat::SpD,
        PokemonStat::Spe,
    ];
    for (i, stat) in STATS.iter().enumerate() {
        let si = i + 1;
        let bsv_lo = calc_stat(new_base[si], mon.min_ivs[si], mon.min_evs[si], level, 1.0);
        let bsv_hi = calc_stat(new_base[si], mon.max_ivs[si], mon.max_evs[si], level, 1.0);
        mon.min_pre_nature_stat[si] = bsv_lo;
        mon.max_pre_nature_stat[si] = bsv_hi;

        // Widest nature modifier still possible for this stat, over the mon's current
        // (possibly already-narrowed) nature candidates.
        let classes = possible_nature_classes(&mon.possible_natures, stat, si);
        let (min_mod, max_mod) = classes.iter().fold(
            (f32::MAX, f32::MIN),
            |(mn, mx), &(m, _, _)| (mn.min(m), mx.max(m)),
        );
        mon.min_stats[si] = (bsv_lo as f64 * min_mod as f64).floor() as u16;
        mon.max_stats[si] = (bsv_hi as f64 * max_mod as f64).floor() as u16;
    }
}

fn apply_switch_state_to_mon(
    mon: &mut UnknownPokemonState,
    sw: &SwitchState,
    config: &InferenceConfig,
) {
    mon.level = sw.level;
    mon.hp = sw.hp.clone();
    mon.status = sw.status.clone();
    mon.switched_in_this_turn = true;
    mon.entered_this_turn = true;
    // Clear per-field flags on switch-in (mirrors helpers.rs:5396-5399).
    mon.first_move_on_field = true;
    mon.first_turn_on_field_pending = false; // caller can override for mid-turn entries
    mon.used_moves_this_field = [false; 4];
    if let Some(tt) = &sw.tera_type {
        mon.is_tera = true;
        mon.possible_tera_type = Unknown::Known(tt.clone());
    }
    // IV range is set by recompute_stats_for_iv_mode; apply_switch_state_to_mon only
    // enforces the flag for mons that arrive from back (already built without force_max).
    if config.force_max_ivs {
        mon.min_ivs = [31; 6];
        mon.max_ivs = [31; 6];
    }
}

/// Infer or exclude `Regenerator` by comparing the mon's last-known benched HP
/// against the incoming HP observed in `sw`. Regenerator heals `floor(max_hp / 3)`
/// silently on switch-out (no `Healed`/`AbilityRevealed` event) — the only observable
/// is a ≈33% higher return HP (±2% from `hp_to_percent` rounding). See the inline
/// guards below for the exact skip conditions (hazards, near-full HP, own-side
/// redundancy).
///
/// Also updates `possible_original_abilities` so the inference survives future switch
/// cycles, which reset `possible_abilities` from it via `apply_switch_out_reset`.
fn infer_regenerator_from_hp_delta(
    mon: &mut UnknownPokemonState,
    sw: &SwitchState,
    state: &UnknownBattleState,
) {
    // Skip if ability is already certain.
    if matches!(mon.possible_abilities, Unknown::Known(_)) {
        return;
    }

    // Only meaningful for Percent HP (opponent's perspective).
    let (h_out, h_in) = match (&mon.hp, &sw.hp) {
        (PokemonHP::Percent(out), PokemonHP::Percent(r#in)) => (*out as i16, *r#in as i16),
        _ => return,
    };

    // Skip when entry hazards are present on the entering side. Since the emission
    // fix, the Switch event carries PRE-hazard HP (the chip arrives as a nested
    // DamageDealt), so this guard is no longer strictly needed — kept as a
    // conservative belt-and-braces (skipping only costs completeness, never soundness).
    let entering_conditions = match sw.slot.player {
        Player::P1 => &state.p1_side_conditions,
        Player::P2 => &state.p2_side_conditions,
    };
    let has_hazards = entering_conditions.iter().any(|sc| {
        matches!(sc, SideCondition::StealthRock | SideCondition::Spikes(_) | SideCondition::ToxicSpikes(_))
    });
    if has_hazards {
        return;
    }

    // If the mon left at > 66% HP, Regenerator's heal would push it to ≥100% (cap)
    // and the re-entry HP cannot distinguish Regenerator from any other scenario.
    // The threshold is conservative: regen heal ≈ 33%, 66 + 33 = 99 < 100.
    if h_out > 66 {
        return;
    }

    let delta = h_in - h_out;

    // Regenerator heal ≈ 33% in percent terms (±2% for rounding).
    // A gain in [31, 35] is consistent with Regen and inconsistent with no-Regen.
    const REGEN_GAIN_MIN: i16 = 31;
    const REGEN_GAIN_MAX: i16 = 35;
    // Without Regen the HP should not change (HP changes between turns on the bench
    // other than Regen and hazards, which we've excluded).  Allow ±1 for rounding.
    const NO_REGEN_TOLERANCE: i16 = 1;

    if (REGEN_GAIN_MIN..=REGEN_GAIN_MAX).contains(&delta) {
        // HP gain matches Regenerator; Regenerator is the only benched-heal ability.
        if !unknown_is_excluded(&mon.possible_abilities, &Ability::Regenerator) {
            unknown_set_known(
                &mut mon.possible_abilities,
                Ability::Regenerator,
                "regenerator-hp-gain",
            );
            unknown_set_known(
                &mut mon.possible_original_abilities,
                Ability::Regenerator,
                "regenerator-hp-gain",
            );
        }
    } else if delta.unsigned_abs() <= NO_REGEN_TOLERANCE as u16 {
        // No HP gain observed; a Regenerator gain would have been distinguishable.
        unknown_exclude(
            &mut mon.possible_abilities,
            &Ability::Regenerator,
            "regenerator-absence",
        );
        unknown_exclude(
            &mut mon.possible_original_abilities,
            &Ability::Regenerator,
            "regenerator-absence",
        );
    }
    // Delta outside both windows (e.g. partial gain from some other effect): do nothing.
}

/// Apply the bench (switch-out) reset to whatever mon is currently in `slot`, if any.
/// Mirrors the switch-out field clearing at `simulator/mod.rs:6225-6246`.
pub(crate) fn apply_switch_out_reset(state: &mut UnknownBattleState, slot: &FieldSlot) {
    let actives = match slot.player {
        Player::P1 => &mut state.p1_active_mons,
        Player::P2 => &mut state.p2_active_mons,
    };
    let i = slot.slot_index as usize;
    if let Some(mon) = actives.get_mut(i) {
        // S26: revert a Transform/Imposter copy first (mirrors the sim's
        // apply_switch_out_ability_effects). Restore the saved pre-transform snapshot
        // but preserve the live HP / status / fainted flag (damage and status taken
        // while transformed carry over); the boost/volatile clears below then apply
        // to the reverted mon exactly as the sim clears them after reverting.
        if let Some(saved) = mon.pre_transform.take() {
            let live_hp = mon.hp.clone();
            let live_status = mon.status.clone();
            let live_fainted = mon.fainted;
            *mon = *saved;
            mon.hp = live_hp;
            mon.status = live_status;
            mon.fainted = live_fainted;
        }
        // Clear all stat boosts (mirrors simulator clear_pokemon_for_switch_out:6206).
        mon.boosts.iter_mut().for_each(|b| *b = 0);
        // Clear all volatile statuses (mirrors simulator:6205).
        mon.volatiles.clear();
        // Reset ToxicPoison tier to 0 on switch-out (mirrors simulator:6213-6214).
        if matches!(mon.status, Some(Status::ToxicPoison(_))) {
            mon.status = Some(Status::ToxicPoison(0));
        }
        // Entry / field flags that don't persist on the bench.
        mon.entered_this_turn = false;
        mon.first_move_on_field = false;
        mon.first_turn_on_field_pending = false;
        mon.cud_chew_pending = None;
        // Unburden ends on switch-out.
        mon.item_lost = false;
        // Per-turn event flags don't follow to the bench.
        mon.damaged_this_turn = false;
        mon.damaged_by_this_turn.clear();
        mon.last_physical_damage_taken = PokemonHP::Percent(0);
        mon.last_physical_attacker = None;
        mon.last_special_damage_taken = PokemonHP::Percent(0);
        mon.last_special_attacker = None;
        mon.last_damage_taken = PokemonHP::Percent(0);
        mon.last_damage_attacker = None;
        mon.stats_raised_this_turn = false;
        mon.stats_lowered_this_turn = false;
        mon.switched_in_this_turn = false;
        // Consecutive-use streaks reset on switch-out.
        mon.stall_counter = 0;
        mon.ally_switch_counter = 0;
        mon.consecutive_move_count = 0;
        // Null last_used_move so the Metronome streak doesn't carry across switch-ins.
        mon.last_used_move = None;
        // Rage Fist hit counter resets (Champions rules).
        mon.times_hit = 0;
        // Live ability resets to the innate ability set on switch-out.
        // Trace / Skill Swap / Mummy / etc. do not persist across a switch.
        mon.possible_abilities = mon.possible_original_abilities.clone();

        // Keep a live Zoroark sub-state's own physically-observable fields in
        // lockstep — it describes the same physical mon, just under a different
        // identity hypothesis (see `possible_illusion_state`'s doc comment).
        mirror_infallible_on_illusion(mon, |sub| {
            sub.boosts.iter_mut().for_each(|b| *b = 0);
            sub.volatiles.clear();
            if matches!(sub.status, Some(Status::ToxicPoison(_))) {
                sub.status = Some(Status::ToxicPoison(0));
            }
            sub.entered_this_turn = false;
            sub.first_move_on_field = false;
            sub.first_turn_on_field_pending = false;
            sub.cud_chew_pending = None;
            sub.item_lost = false;
            sub.damaged_this_turn = false;
            sub.damaged_by_this_turn.clear();
            sub.last_physical_damage_taken = PokemonHP::Percent(0);
            sub.last_physical_attacker = None;
            sub.last_special_damage_taken = PokemonHP::Percent(0);
            sub.last_special_attacker = None;
            sub.last_damage_taken = PokemonHP::Percent(0);
            sub.last_damage_attacker = None;
            sub.stats_raised_this_turn = false;
            sub.stats_lowered_this_turn = false;
            sub.switched_in_this_turn = false;
            sub.stall_counter = 0;
            sub.ally_switch_counter = 0;
            sub.consecutive_move_count = 0;
            sub.last_used_move = None;
            sub.times_hit = 0;
            sub.possible_abilities = sub.possible_original_abilities.clone();
        });
    }
}

fn reveal_move_on_mon(mon: &mut UnknownPokemonState, pokemon_move: &PokemonMove) {
    if mon
        .known_moves
        .iter()
        .any(|m| m.as_ref() == Some(pokemon_move))
    {
        return; // already known
    }
    for slot in mon.known_moves.iter_mut() {
        if slot.is_none() {
            *slot = Some(pokemon_move.clone());
            return;
        }
    }
    // All 4 slots filled but move not found — legal mon constraint violated.
    // Don't panic; widening is sound.
}

/// Confirms `move_used` is legally learnable by `mon`'s (single, `Known`) species
/// — panics via `inference_contradiction!` if the species' learnset is known and
/// does NOT include the move (a genuine impossibility for a real, unrevealed
/// Pokémon of that species to have used it). Absent learnset data is NOT treated
/// as a contradiction (sound: absence of data isn't evidence of inability — same
/// rule the old `narrow_species_by_learnset` documented). A no-op when
/// `mon.possible_species` isn't `Known` or `learnset_dex` is empty (learnset
/// narrowing disabled).
///
/// This is the fallible half of move-reveal handling for the Zoroark parallel-
/// hypothesis model: called alongside `reveal_move_on_mon` (which just records
/// the move and never itself panics) through `apply_with_illusion_mirroring`, so
/// a move outside a hypothesis' learnset drops that hypothesis, and a move
/// outside the PRIMARY's own learnset (while the hypothesis' learnset accepts
/// it) promotes the mon to that hypothesis instead.
fn check_move_legal_for_species(
    mon: &UnknownPokemonState,
    move_used: &PokemonMove,
    learnset_dex: &HashMap<Species, HashSet<PokemonMove>>,
) {
    if learnset_dex.is_empty() {
        return;
    }
    let Unknown::Known(species) = &mon.possible_species else { return };
    if let Some(moves) = learnset_dex.get(species)
        && !moves.contains(move_used)
    {
        inference_contradiction!(
            species,
            "species cannot learn revealed move {:?}",
            move_used
        );
    }
}

/// Narrow `possible_species` (when `Possibly`) by excluding candidates whose learnset
/// doesn't include `move_used`. Collapses to `Known` when only one candidate remains
/// and refreshes `possible_types` / `possible_weight_hg` from the species dex.
///
/// Sound: only removes a species if we have *positive* learnset data confirming it
/// cannot learn the move.  Absent learnset data → keeps the candidate.
fn narrow_species_by_learnset(
    mon: &mut UnknownPokemonState,
    move_used: &PokemonMove,
    learnset_dex: &HashMap<Species, HashSet<PokemonMove>>,
    dex: &HashMap<Species, PokemonData>,
) {
    if learnset_dex.is_empty() {
        return;
    }
    let candidates = match &mon.possible_species {
        Unknown::Possibly(v) => v.clone(),
        _ => return, // Known → nothing to narrow; Not → can't enumerate safely
    };

    let remaining: Vec<Species> = candidates
        .iter()
        .filter(|s| {
            // Sound: keep species if learnset data is absent (can't confirm illegality).
            learnset_dex.get(*s).is_none_or(|moves| moves.contains(move_used))
        })
        .cloned()
        .collect();

    if remaining.len() == candidates.len() {
        return; // Nothing excluded.
    }
    if remaining.is_empty() {
        // All candidates illegal — learnset data may be wrong; don't narrow to contradiction.
        return;
    }

    if remaining.len() == 1 {
        let species = remaining[0].clone();
        mon.possible_species = Unknown::Known(species.clone());
        // Refresh types and weight from the now-pinned species.
        if let Some(pd) = dex.get(&species) {
            mon.possible_types = Unknown::Known(pd.types.clone());
            mon.possible_weight_hg = Unknown::Known(pd.weight);
        }
    } else {
        mon.possible_species = Unknown::Possibly(remaining);
    }
}

/// Exclude Choice items when the mon has used 2+ different moves in the same field stint.
///
/// Uses `used_moves_this_field` (cleared on switch-in) rather than `last_used_move`
/// (not cleared on switch-out) so that a Pokémon using a new move after switching back
/// in is never incorrectly flagged as Choice-locked.
///
/// S20: skipped once the mon's held item arrived via a mid-battle transfer
/// (`item_was_transferred`, set by `ItemGained` — Trick / Switcheroo / Recycle /
/// Pickup). Choice lock binds from the first move used *while holding* the Choice
/// item, so "used move A, was Tricked a Choice Scarf, then legally picked move B" is
/// a perfectly consistent sequence — excluding the Scarf from the mon's now-`Known`
/// item was a guaranteed contradiction panic. (An item *loss* mid-stint needs no
/// guard: `ItemLost` pins the item to `Known(None)`, on which the exclusion no-ops.)
///
/// Call AFTER `used_moves_this_field` has been updated for `new_move`.
fn pass1_choice_exclusion(mon: &mut UnknownPokemonState, new_move: &PokemonMove) {
    if mon.item_was_transferred {
        return;
    }
    // Count how many distinct known moves have been used this field.
    let distinct_used: Vec<&PokemonMove> = mon
        .known_moves
        .iter()
        .enumerate()
        .filter_map(|(i, slot)| {
            if mon.used_moves_this_field[i] {
                slot.as_ref()
            } else {
                None
            }
        })
        .collect();

    // If the mon has used more than one distinct move since it came in, Choice items are out.
    let has_different = distinct_used.iter().any(|&m| m != new_move);
    if has_different && distinct_used.len() >= 2 {
        let choices = [Item::ChoiceBand, Item::ChoiceScarf, Item::ChoiceSpecs];
        for ci in &choices {
            unknown_exclude(&mut mon.item, ci, "choice-lock");
        }
    }
}

fn update_mon_hp(state: &mut UnknownBattleState, slot: &FieldSlot, new_hp: PokemonHP) {
    if let Some(idx) = mon_idx_for_active_slot(state, slot)
        && let Some(mon) = get_mon_mut_by_idx(state, idx) {
            mon.hp = new_hp;
        }
}

// ── Ability suppression helpers ────────────────────────────────────────────────

/// `true` if this mon's ability is DEFINITELY suppressed (Gastro Acid volatile present).
/// Does NOT check for Neutralizing Gas because we only call this when the active
/// field scan (below) already cleared the NeutralizingGas check.
fn has_gastro_acid(mon: &UnknownPokemonState) -> bool {
    mon.volatiles.iter().any(|v| {
        matches!(v,
            VolatileStatusState::TurnStatus(VolatileStatus::GastroAcid, _)
            | VolatileStatusState::MoveStatus(VolatileStatus::GastroAcid, _))
    })
}

/// `true` if we can be CERTAIN that some active mon has Neutralizing Gas (meaning
/// that all non-NeutralizingGas abilities are suppressed field-wide). We are
/// certain only when `possible_abilities == Known(NeutralizingGas)`.
fn neutralizing_gas_definitely_active(state: &UnknownBattleState) -> bool {
    state
        .p1_active_mons
        .iter()
        .chain(state.p2_active_mons.iter())
        .any(|m| {
            !m.fainted
                && unknown_is_known_as(&m.possible_abilities, &Ability::NeutralizingGas)
        })
}

/// `true` if this mon's ability might be suppressed (sound: returns true whenever
/// suppression is possible, not just certain). Used to skip absence-of-effect inference.
fn unknown_ability_might_be_suppressed(state: &UnknownBattleState, slot: &FieldSlot) -> bool {
    // If Neutralizing Gas might be on the field → suppress inference (sound: might be true).
    let maybe_ng = state
        .p1_active_mons
        .iter()
        .chain(state.p2_active_mons.iter())
        .any(|m| {
            !m.fainted
                && !unknown_is_excluded(&m.possible_abilities, &Ability::NeutralizingGas)
        });
    if maybe_ng {
        return true;
    }
    // Check per-mon Gastro Acid.
    if let Some(idx) = mon_idx_for_active_slot(state, slot)
        && let Some(mon) = get_mon_by_idx(state, idx) {
            return has_gastro_acid(mon);
        }
    false
}

/// True if any active mon (either side) could have Air Lock or Cloud Nine. Those abilities
/// suspend weather *effects* — including the sandstorm EOT chip — while the weather itself
/// stays set (the simulator's `current_weather` returns `None`, but `state.weather` is
/// unchanged). Used to skip weather-effect absence inference: absence of the sand chip may
/// simply mean the effect is suspended, not that the mon is immune (sound: might be true).
fn weather_effects_might_be_suspended(state: &UnknownBattleState) -> bool {
    state
        .p1_active_mons
        .iter()
        .chain(state.p2_active_mons.iter())
        .any(|m| {
            !m.fainted
                && (!unknown_is_excluded(&m.possible_abilities, &Ability::AirLock)
                    || !unknown_is_excluded(&m.possible_abilities, &Ability::CloudNine))
        })
}

// ── End-of-turn bookkeeping ────────────────────────────────────────────────────

/// Return the item that extends a weather effect beyond its base 5-turn duration.
/// Used by I-A to reveal the rock item when the weather timer confirms the 8-turn branch.
fn weather_extension_item(weather: &Weather) -> Option<Item> {
    match weather {
        Weather::Rain => Some(Item::DampRock),
        Weather::Sun => Some(Item::HeatRock),
        Weather::Sandstorm => Some(Item::SmoothRock),
        Weather::Snow => Some(Item::IcyRock),
        // Primordial weathers: timer is Known(0) sentinel; never emitted.
        Weather::HeavyRain | Weather::ExtremeSunlight | Weather::StrongWinds => None,
    }
}

/// Return the item that extends a terrain effect beyond its base 5-turn duration.
fn terrain_extension_item(_terrain: &Terrain) -> Item {
    Item::TerrainExtender
}

/// Return the item that extends a screen beyond its base 5-turn duration.
fn screen_extension_item(sc: &SideCondition) -> Option<Item> {
    match sc {
        SideCondition::Reflect | SideCondition::LightScreen | SideCondition::AuroraVeil => {
            Some(Item::LightClay)
        }
        _ => None,
    }
}

/// Emit `HasItem{mon_idx, item}` as a `Known` fact when a timer has just
/// collapsed from `Possibly` to `Known(n > 0)`, confirming the extended duration.
/// This is a pure narrowing (soundness: only fires when the longer branch is
/// the ONLY remaining candidate) and is always guaranteed information.
fn emit_extension_item_if_collapsed(
    state: &mut UnknownBattleState,
    was_possibly: bool,
    setter_idx: Option<usize>,
    item: Item,
) {
    if !was_possibly {
        return; // Not a collapse; timer was already Known or was Never decremented.
    }
    // After decrement, if a Possibly collapsed to Known it will now be Known(n).
    // We don't re-check the value here — the caller verifies n > 0.
    if let Some(idx) = setter_idx
        && let Some(mon) = get_mon_mut_by_idx(state, idx) {
            unknown_set_known(&mut mon.item, item, "ia-extension-item");
        }
}

/// Apply internal EOT resets that mirror `end_turn` Phase 5 and
/// `decrement_effect_timers` in `simulator/helpers.rs`. Visible EOT effects (weather
/// chip, heal, etc.) are handled by the event walk visiting `EndOfTurn::reactions`;
/// this function handles the invisible internal state.
fn apply_end_of_turn(state: &mut UnknownBattleState, event: &InformationEvent) {
    // ── Decrement field timers and detect Possibly→Known collapses (I-A) ─────
    //
    // SOUNDNESS: at the very end-of-turn where a Possibly([1, 4]) timer collapses to
    // Known(3), the base-duration world would have the effect END at this same EOT.
    // The end event (WeatherChanged{None} / TerrainChanged{None} / SideConditionEnd)
    // sits in this EndOfTurn's *reactions*, which pass 1 has not yet processed — so
    // before revealing the extension item we must check the reactions: if the effect
    // ends this turn, the base track was true and the correct inference is the
    // OPPOSITE — exclude the extension item on the setter (natural expiry).

    // Weather.  Three-way resolution at the collapse turn:
    //   - ended (WeatherChanged{None} in this EOT's reactions): natural expiry at the
    //     base duration → the setter has NO extension rock (exclude);
    //   - overridden (WeatherChanged{Some(_)}): the old weather was replaced before its
    //     duration resolved — no information either way (do nothing; the WeatherChanged
    //     handler purges the pending turn-count clauses);
    //   - persists: the base-duration track is dead → the setter HAS the rock (reveal).
    let weather_was_possibly = matches!(&state.weather_turns, Some(Unknown::Possibly(_)));
    let weather_setter = state.weather_setter_mon_idx;
    let weather_type_snap = state.weather.clone();
    let weather_ended = any_reaction_deep(&event.reactions, &|k| {
        matches!(k, EventKind::WeatherChanged { weather: None })
    });
    let weather_overridden = any_reaction_deep(&event.reactions, &|k| {
        matches!(k, EventKind::WeatherChanged { weather: Some(_) })
    });
    decrement_unknown_turns(&mut state.weather_turns, &mut state.weather);
    if weather_was_possibly
        && let Some(Unknown::Known(n)) = &state.weather_turns
            && *n > 0
                && let Some(weather) = &weather_type_snap
                    && let Some(rock) = weather_extension_item(weather) {
                        if weather_ended {
                            // Natural expiry at base duration → the setter has NO rock.
                            if let Some(idx) = weather_setter
                                && let Some(mon) = get_mon_mut_by_idx(state, idx) {
                                    unknown_exclude(&mut mon.item, &rock, "natural-expiry-no-rock");
                                }
                        } else if !weather_overridden {
                            // Extended duration confirmed (8-turn branch survived).
                            emit_extension_item_if_collapsed(state, true, weather_setter, rock);
                        }
                    }

    // Terrain (same three-way resolution as weather).
    let terrain_was_possibly = matches!(&state.terrain_turns, Some(Unknown::Possibly(_)));
    let terrain_setter = state.terrain_setter_mon_idx;
    let terrain_ended = any_reaction_deep(&event.reactions, &|k| {
        matches!(k, EventKind::TerrainChanged { terrain: None })
    });
    let terrain_overridden = any_reaction_deep(&event.reactions, &|k| {
        matches!(k, EventKind::TerrainChanged { terrain: Some(_) })
    });
    decrement_unknown_turns(&mut state.terrain_turns, &mut state.terrain);
    if terrain_was_possibly
        && let Some(Unknown::Known(n)) = &state.terrain_turns
            && *n > 0 {
                let extender = terrain_extension_item(&Terrain::ElectricTerrain); // any Terrain
                if terrain_ended {
                    if let Some(idx) = terrain_setter
                        && let Some(mon) = get_mon_mut_by_idx(state, idx) {
                            unknown_exclude(&mut mon.item, &extender, "natural-expiry-no-extender");
                        }
                } else if !terrain_overridden {
                    emit_extension_item_if_collapsed(state, true, terrain_setter, extender);
                }
            }

    for t in state.pseudo_weather_turns.iter_mut() {
        decrement_unknown_turns_raw(t);
    }
    // Remove expired pseudo-weathers (those whose turn set collapsed to empty).
    // (We don't know which pseudo-weather expired — leave for event-driven clearing.)

    // P1 / P2 side conditions (same expiry-vs-collapse logic as weather above).
    for player in [Player::P1, Player::P2] {
        let count = match player {
            Player::P1 => state.p1_side_conditions.len(),
            Player::P2 => state.p2_side_conditions.len(),
        };
        for i in 0..count {
            let (was_possibly, sc) = {
                let (conditions, turns) = match player {
                    Player::P1 => (&state.p1_side_conditions, &mut state.p1_side_condition_turns),
                    Player::P2 => (&state.p2_side_conditions, &mut state.p2_side_condition_turns),
                };
                let was_possibly = matches!(&turns[i], Unknown::Possibly(_));
                decrement_unknown_turns_raw(&mut turns[i]);
                (was_possibly, conditions[i].clone())
            };
            let collapsed_positive = {
                let turns = match player {
                    Player::P1 => &state.p1_side_condition_turns,
                    Player::P2 => &state.p2_side_condition_turns,
                };
                matches!(&turns[i], Unknown::Known(n) if *n > 0)
            };
            if was_possibly && collapsed_positive
                && let Some(clay) = screen_extension_item(&sc) {
                    let setter = match player {
                        Player::P1 => state.p1_side_condition_setters.get(i).copied().flatten(),
                        Player::P2 => state.p2_side_condition_setters.get(i).copied().flatten(),
                    };
                    // Screens have no "override" case (re-setting fails while up), and
                    // forced removals (Brick Break / Defog / Court Change) happen at
                    // move time, never nested under EndOfTurn — so an EOT-nested
                    // SideConditionEnd for this exact side+condition is a natural expiry.
                    let ends_this_turn = any_reaction_deep(&event.reactions, &|k| {
                        matches!(k, EventKind::SideConditionEnd { side, condition }
                            if *side == player && *condition == sc)
                    });
                    if ends_this_turn {
                        // Natural expiry at base duration → the setter has no Light Clay.
                        if let Some(idx) = setter
                            && let Some(mon) = get_mon_mut_by_idx(state, idx) {
                                unknown_exclude(&mut mon.item, &clay, "natural-expiry-no-clay");
                            }
                    } else {
                        emit_extension_item_if_collapsed(state, true, setter, clay);
                    }
                }
        }
    }

    // Also decrement the `turns` field inside any turn-count predicate.
    for clause in state.predicates.iter_mut() {
        for lit in clause.iter_mut() {
            match lit {
                Statement::WeatherTurns { turns }
                | Statement::TerrainTurns { turns }
                | Statement::SideConditionTurns { turns, .. } => {
                    if *turns > 0 {
                        *turns -= 1;
                    }
                }
                _ => {}
            }
        }
    }
    // Remove predicates whose turns have reached 0: by then the timer machinery has
    // already extracted the information (collapse reveal / natural-expiry exclusion),
    // so the clause is spent.
    state.predicates.retain(|clause| {
        !clause.iter().any(|lit| {
            matches!(
                lit,
                Statement::WeatherTurns { turns: 0 }
                    | Statement::TerrainTurns { turns: 0 }
                    | Statement::SideConditionTurns { turns: 0, .. }
            )
        })
    });

    // ── Advance turn counter ──────────────────────────────────────────────────
    state.turn_number = state.turn_number.saturating_add(1);

    // ── Clear per-turn flags (mirrors end_turn Phase 5, helpers.rs:6623-6673) ─
    for mon in state
        .p1_active_mons
        .iter_mut()
        .chain(state.p2_active_mons.iter_mut())
    {
        mon.entered_this_turn = false;
        // U-turn / self-switch mid-turn: first_turn_on_field_pending causes EOT to skip
        // clearing first_move_on_field exactly once.
        if mon.first_turn_on_field_pending {
            mon.first_turn_on_field_pending = false;
        } else {
            mon.first_move_on_field = false;
        }
        mon.damaged_this_turn = false;
        mon.damaged_by_this_turn.clear();
        mon.last_physical_damage_taken = PokemonHP::Percent(0);
        mon.last_physical_attacker = None;
        mon.last_special_damage_taken = PokemonHP::Percent(0);
        mon.last_special_attacker = None;
        mon.last_damage_taken = PokemonHP::Percent(0);
        mon.last_damage_attacker = None;
        mon.stats_raised_this_turn = false;
        mon.stats_lowered_this_turn = false;
        mon.switched_in_this_turn = false;
        // Turn-scoped volatiles (Roost, Electrify).
        mon.volatiles.retain(|v| {
            !matches!(v,
                VolatileStatusState::TurnStatus(VolatileStatus::Roost, _)
                | VolatileStatusState::TurnStatus(VolatileStatus::Electrify, _)
                | VolatileStatusState::MoveStatus(VolatileStatus::Roost, _)
                | VolatileStatusState::MoveStatus(VolatileStatus::Electrify, _))
        });
    }
    state.round_used_this_turn = false;
    state.items_consumed_this_turn.clear();
}

/// Decrement an `Unknown<u8>` field representing remaining effect turns.
/// If the counter reaches 0 in all possibilities, clears the option to reflect expiry.
fn decrement_unknown_turns<T>(turns_opt: &mut Option<Unknown<u8>>, field: &mut Option<T>) {
    if let Some(t) = turns_opt.as_mut() {
        decrement_unknown_turns_raw(t);
        // If the Possibly set is now empty, all possibilities say the effect has expired.
        // We leave clearing `field` to the event-driven path (WeatherChanged / SideConditionEnd)
        // so that we don't accidentally clear weather that persisted (8-turn case).
    }
}

// ── Per-effect timer models ───────────────────────────────────────────────────
//
// Soundness requirement: the candidate set must be a *superset* of every true
// duration the game can produce.  Where an item can extend the duration (weather
// rocks, Light Clay, Terrain Extender) we use `Possibly([5,8])`; where the
// duration is fixed by mechanic (not by item), we use `Known(n)`.
//
// `Known(0)` is the "permanent / no countdown" sentinel used for primordial
// weathers and entry hazards.  `decrement_unknown_turns_raw` is a no-op on 0,
// and the predicate machinery never emits turn-count clauses for these effects.
//
// Durations confirmed against Bulbapedia (newest-generation behaviour):
//   Tailwind=4 (Gen V+), Screens=5/8 (Light Clay), Tricks/Gravity/WR=5,
//   Safeguard/Mist/Lucky Chant=5, one-turn guards=1, FairyLock/IonDeluge=1,
//   Mud/Water Sport=5, MagicDeluge(Magic Room)=5.

/// Timer model for a newly-set weather effect.
fn weather_timer(w: &Weather) -> Unknown<u8> {
    match w {
        // Standard weathers: base 5; Heat/Damp/Smooth/Icy Rock extends to 8.
        Weather::Rain | Weather::Sun | Weather::Sandstorm | Weather::Snow => {
            Unknown::Possibly(vec![5, 8])
        }
        // Primordial weathers (from Abilities): never tick down.
        Weather::HeavyRain | Weather::ExtremeSunlight | Weather::StrongWinds => {
            Unknown::Known(0)
        }
    }
}

/// Timer model for a newly-set terrain effect.
fn terrain_timer(_t: &Terrain) -> Unknown<u8> {
    // All terrains: base 5; Terrain Extender extends to 8.
    Unknown::Possibly(vec![5, 8])
}

/// Timer model for a newly-active pseudo-weather effect.
fn pseudo_weather_timer(pw: &PseudoWeather) -> Unknown<u8> {
    match pw {
        // All 5-turn pseudo-weathers (no item extension exists).
        PseudoWeather::TrickRoom
        | PseudoWeather::Gravity
        | PseudoWeather::WonderRoom
        | PseudoWeather::MudSport
        | PseudoWeather::WaterSport
        | PseudoWeather::MagicDeluge => Unknown::Known(5),
        // One-turn effects.
        PseudoWeather::FairyLock | PseudoWeather::IonDeluge => Unknown::Known(1),
    }
}

/// Timer model for a newly-active side condition.
fn side_condition_timer(sc: &SideCondition) -> Unknown<u8> {
    match sc {
        // Screens: base 5; Light Clay extends to 8.
        SideCondition::Reflect
        | SideCondition::LightScreen
        | SideCondition::AuroraVeil => Unknown::Possibly(vec![5, 8]),
        // Tailwind: exactly 4 turns (Gen V+).  Formerly Possibly([5,8]) — UNSOUND.
        SideCondition::TailWind => Unknown::Known(4),
        // Fixed 5-turn side conditions.
        SideCondition::SafeGuard
        | SideCondition::Mist
        | SideCondition::LuckyChant => Unknown::Known(5),
        // One-turn protections (expire at end of the turn they are used).
        SideCondition::QuickGuard
        | SideCondition::WideGuard
        | SideCondition::CraftyShield
        | SideCondition::MatBlock => Unknown::Known(1),
        // Entry hazards: permanent until cleared (no countdown).
        SideCondition::Spikes(_)
        | SideCondition::StealthRock
        | SideCondition::StickyWeb(_)
        | SideCondition::ToxicSpikes(_) => Unknown::Known(0),
    }
}

fn decrement_unknown_turns_raw(t: &mut Unknown<u8>) {
    match t {
        Unknown::Known(n) => {
            if *n > 0 {
                *n -= 1;
            }
        }
        Unknown::Possibly(v) => {
            *v = v.iter().filter_map(|&n| if n > 1 { Some(n - 1) } else { None }).collect();
            if v.len() == 1 {
                *t = Unknown::Known(v[0]);
            }
        }
        Unknown::Not(_) => {} // Not meaningful for turn counts
    }
}

// ── Ability absence / priority inference ──────────────────────────────────────

/// Weather-setting abilities whose activation is always visible (`WeatherChanged`) —
/// *unless* the weather they'd set is already active, in which case `set_weather` no-ops
/// and no `WeatherChanged` fires (see `weather_setting_ability_target` below).
const WEATHER_SETTING_ABILITIES: &[Ability] = &[
    Ability::Drizzle,
    Ability::Drought,
    Ability::SandStream,
    Ability::SnowWarning,
    Ability::OrichalcumPulse, // Sets Sun (apply_entry_ability_field_effects, helpers.rs)
];

/// The weather a given weather-setting ability would install, or `None` if `ab` isn't one
/// of `WEATHER_SETTING_ABILITIES`. Mirrors `apply_entry_ability_field_effects` in
/// `simulator/helpers.rs`. Used to guard the absence-inference pass below: since a weather
/// ability re-activating under its own already-active weather now (correctly) skips
/// `set_weather` and emits no `WeatherChanged`, that specific ability's absence can no
/// longer be inferred from "no WeatherChanged" alone in that case — but every *other*
/// weather-setting ability still can (they'd have changed the weather and didn't).
fn weather_setting_ability_target(ab: &Ability) -> Option<Weather> {
    match ab {
        Ability::Drizzle => Some(Weather::Rain),
        Ability::Drought | Ability::OrichalcumPulse => Some(Weather::Sun),
        Ability::SandStream => Some(Weather::Sandstorm),
        Ability::SnowWarning => Some(Weather::Snow),
        _ => None,
    }
}

/// Terrain-setting abilities whose activation is always visible (`TerrainChanged`).
const TERRAIN_SETTING_ABILITIES: &[Ability] = &[
    Ability::ElectricSurge,
    Ability::GrassySurge,
    Ability::MistySurge,
    Ability::PsychicSurge,
    // S7: HadronEngine sets Electric Terrain, NOT weather — it must live in this list,
    // not WEATHER_SETTING_ABILITIES. `apply_entry_ability_field_effects` matches
    // `Ability::ElectricSurge | Ability::HadronEngine => set_terrain(..., ElectricTerrain, ...)`;
    // it never touches `state.weather` at all, so "no WeatherChanged" carries zero
    // information about whether the entrant has HadronEngine (the absence would be
    // true regardless), and excluding it there was vacuous, not sound evidence.
    Ability::HadronEngine,
];

/// Returns `true` when every active Pokémon on the side opposing `entering_slot` would
/// produce a visible `BoostChanged { Atk, −1 }` reaction if Intimidate had fired;
/// `false` (Intimidate cannot be safely excluded) when at least one foe would silently
/// block/redirect/invert the drop, or already sits at −6 Atk (the drop is a no-op).
///
/// Abilities that prevent a visible −1: `InnerFocus/OwnTempo/Oblivious/Scrappy` (full
/// immunity), `ClearBody/WhiteSmoke/FullMetalBody` (blocked, only `AbilityRevealed`
/// fires), `HyperCutter` (blocks Atk drops specifically), `GuardDog` (converts to +1),
/// `Contrary` (inverts to +1), `MirrorArmor` (bounces onto the Intimidate user instead).
///
/// Observer's own mons always have `Known` abilities, so checking `Unknown::Known(ab)` is safe.
fn intimidate_drop_would_be_visible(state: &UnknownBattleState, entering_slot: &FieldSlot) -> bool {
    let foe_mons: &Vec<UnknownPokemonState> = match entering_slot.player {
        Player::P1 => &state.p2_active_mons,
        Player::P2 => &state.p1_active_mons,
    };

    let blocks_visible_drop = |ability: &Ability| {
        matches!(
            ability,
            Ability::InnerFocus
                | Ability::OwnTempo
                | Ability::Oblivious
                | Ability::Scrappy
                | Ability::ClearBody
                | Ability::WhiteSmoke
                | Ability::FullMetalBody
                | Ability::HyperCutter
                | Ability::GuardDog
                | Ability::Contrary
                | Ability::MirrorArmor
        )
    };

    let non_fainted: Vec<&UnknownPokemonState> = foe_mons.iter().filter(|m| !m.fainted).collect();
    // No active non-fainted foes → Intimidate fires but hits nobody; no −1 visible.
    if non_fainted.is_empty() {
        return false;
    }
    for mon in non_fainted {
        // If the foe's ability is Known and blocks/redirects the drop, the −1 won't appear
        // even when Intimidate is present.
        if let Unknown::Known(ref ability) = mon.possible_abilities
            && blocks_visible_drop(ability) {
                return false;
            }
        // Atk already at −6: the drop is clamped to zero, nothing emitted.
        if mon.boosts[0] <= -6 {
            return false;
        }
    }
    true
}

/// After a batch of switch-ins, scan the combined `reactions` list and exclude
/// abilities from `possible_abilities` that must have activated but didn't. Sound:
/// only excludes when the ability's effect would certainly have been visible.
/// Conservative: skipped for multi-mon batches with more than one possible setter.
fn pass1_ability_absence_inference(
    state: &mut UnknownBattleState,
    entered_slots: &[FieldSlot],
    reactions: &[InformationEvent],
    ctx: &BattleContext,
) {
    if entered_slots.is_empty() {
        return;
    }

    // Deep scans: the simulator nests the visible effect (WeatherChanged, BoostChanged)
    // one level down, under the AbilityRevealed wrapper of the ability that caused it —
    // a flat scan of `reactions` misses it and would unsoundly conclude absence.
    let weather_changed = any_reaction_deep(reactions, &|k| {
        matches!(k, EventKind::WeatherChanged { weather: Some(_) })
    });
    let terrain_changed = any_reaction_deep(reactions, &|k| {
        matches!(k, EventKind::TerrainChanged { terrain: Some(_) })
    });

    for slot in entered_slots {
        // Obtain mon index first — needed for both presence-recording and
        // absence inference below.
        let Some(idx) = mon_idx_for_active_slot(state, slot) else {
            continue;
        };

        // ── Intrepid Sword / Dauntless Shield: presence recording ────────────
        // This is a DIRECT observation (we see the +1 boost), not an inference
        // from absence, so it must NOT be gated on the suppression check below.
        // Even if NeutralizingGas might be present, we literally observed the
        // boost fire and must record it to prevent future false-exclusions on
        // re-entry.
        let intrepid_fired = any_reaction_deep(reactions, &|k| {
            matches!(k, EventKind::BoostChanged { target, boost_idx: 0, stages: 1 } if target == slot)
        });
        let dauntless_fired = any_reaction_deep(reactions, &|k| {
            matches!(k, EventKind::BoostChanged { target, boost_idx: 1, stages: 1 } if target == slot)
        });
        if (intrepid_fired || dauntless_fired)
            && let Some(mon) = get_mon_mut_by_idx(state, idx) {
                mon.one_time_ability_used = true;
            }

        // Skip if ability might be suppressed (sound: conservative).
        // Absence inferences below are gated here; presence-recording above is not.
        if unknown_ability_might_be_suppressed(state, slot) {
            continue;
        }

        // ── Weather-setting abilities ────────────────────────────────────────
        // Guard: `set_weather` silently no-ops (no WeatherChanged) whenever the weather it
        // would set is already active — either because strong/primordial weather blocks any
        // change (helpers.rs:5478-5492), or because the ability's own target weather already
        // matches the current one (a normal-weather reactivation, e.g. Sand Stream switching
        // in under an already-active Sandstorm — `apply_entry_ability_field_effects` skips
        // `set_weather` in that case so the timer isn't reset). Either way, absence of
        // WeatherChanged carries no information for that specific ability's presence. This
        // must be checked PER-ABILITY, not blanket: under active Rain, Drizzle legitimately
        // produces no WeatherChanged and can't be excluded, but Drought/SandStream/SnowWarning
        // still would have changed the weather, so their absence remains sound evidence. This
        // also avoids a contradiction-panic: the absence pass runs before the nested
        // AbilityRevealed reaction is processed, so excluding an ability that then gets
        // revealed as Known would be an unsound contradiction.
        let current_is_strong_weather = matches!(
            state.weather,
            Some(Weather::HeavyRain | Weather::ExtremeSunlight | Weather::StrongWinds)
        );
        if !weather_changed {
            // If ONLY this slot's mons could have a weather setter (no other entering
            // mon has one), absence of WeatherChanged proves this mon doesn't have it.
            // For single-entry (Switch) this is always unambiguous.
            let sole_possible_setter = entered_slots.len() == 1
                || only_slot_with_weather_setter(state, entered_slots, slot);

            // Defensive: if this mon's ability is already a direct observation
            // (`Known`, e.g. from an earlier `AbilityRevealed`), absence-inference must
            // never contradict it — a direct reveal always outranks an inferred
            // absence. The `ability_would_no_op` guard below tries to track every way
            // `set_weather` can legitimately no-op, but it reads the belief's own
            // `state.weather`, which can go stale if some ground-truth weather-clearing
            // path ever forgets to emit `WeatherChanged` (see AUDIT.md — this happened
            // for primal-weather departure/Neutralizing-Gas suppression). Rather than
            // rely on that list being exhaustive, short-circuit whenever the mon is
            // already Known: excluding anything from a `Known` value can only ever be a
            // no-op (different ability) or an unsound contradiction (same ability) —
            // and if the ability were genuinely absent, it could never have become
            // `Known` in the first place.
            let already_known = matches!(
                get_mon_by_idx(state, idx).map(|m| &m.possible_abilities),
                Some(Unknown::Known(_))
            );

            if sole_possible_setter && !already_known {
                for ab in WEATHER_SETTING_ABILITIES {
                    let ability_would_no_op = current_is_strong_weather
                        || weather_setting_ability_target(ab).is_some_and(|target| {
                            state.weather.as_ref() == Some(&target)
                        });
                    if ability_would_no_op {
                        continue;
                    }
                    if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                        unknown_exclude(&mut mon.possible_abilities, ab, "ability-absence-weather");
                    }
                }
            }
        }

        // ── Terrain-setting abilities ────────────────────────────────────────
        if !terrain_changed {
            let sole_possible_setter = entered_slots.len() == 1
                || only_slot_with_terrain_setter(state, entered_slots, slot);

            if sole_possible_setter {
                for ab in TERRAIN_SETTING_ABILITIES {
                    // HadronEngine is already in WEATHER_SETTING_ABILITIES (sets elec terrain
                    // but appears as WeatherChanged in some sims); skip duplicates.
                    if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                        unknown_exclude(&mut mon.possible_abilities, ab, "ability-absence-terrain");
                    }
                }
            }
        }

        // ── Intimidate ───────────────────────────────────────────────────────
        // Intimidate fires a BoostChanged {boost_idx:0, stages:-1} on each adjacent foe.
        // Only check when this is the sole possible Intimidate user and no such boost appeared.
        let sole_possible_intimidate = entered_slots.len() == 1
            || only_slot_with_ability(state, entered_slots, slot, &Ability::Intimidate);

        if sole_possible_intimidate {
            // Look for any opponent-side BoostChanged{0,-1} in reactions (deep — the
            // drop nests under the AbilityRevealed{Intimidate} wrapper).
            let intimidate_fired = any_reaction_deep(reactions, &|k| {
                matches!(k, EventKind::BoostChanged { target, boost_idx: 0, stages: -1 }
                    if target.player != slot.player)
            });
            if !intimidate_fired {
                // Only exclude Intimidate when every adjacent foe would have visibly received
                // the −1 Atk drop.  If any foe has Clear Body, Inner Focus, Guard Dog,
                // Mirror Armor, Contrary, etc., no −1 would appear even WITH Intimidate.
                if intimidate_drop_would_be_visible(state, slot)
                    && let Some(mon) = get_mon_mut_by_idx(state, idx) {
                        unknown_exclude(
                            &mut mon.possible_abilities,
                            &Ability::Intimidate,
                            "ability-absence-intimidate",
                        );
                    }
            }
        }

        // ── Intrepid Sword / Dauntless Shield: absence exclusion ────────────
        // `intrepid_fired`/`dauntless_fired` were computed above (before the
        // suppression guard) — only exclude when the boost was NOT seen and
        // no prior use has been recorded.
        if !intrepid_fired && entered_slots.len() == 1
            && let Some(mon) = get_mon_mut_by_idx(state, idx)
                && !mon.one_time_ability_used {
                    unknown_exclude(
                        &mut mon.possible_abilities,
                        &Ability::IntrepidSword,
                        "ability-absence-intrepid",
                    );
                }
        if !dauntless_fired && entered_slots.len() == 1
            && let Some(mon) = get_mon_mut_by_idx(state, idx)
                && !mon.one_time_ability_used {
                    unknown_exclude(
                        &mut mon.possible_abilities,
                        &Ability::DauntlessShield,
                        "ability-absence-dauntless",
                    );
                }
    }
}

/// Returns `true` if no other slot in `entered_slots` (besides `this_slot`) could
/// possibly hold any of the given `abilities`.  Used to attribute field-setting
/// abilities to a unique entering mon.
fn only_slot_with_abilities(
    state: &UnknownBattleState,
    entered_slots: &[FieldSlot],
    this_slot: &FieldSlot,
    abilities: &[Ability],
) -> bool {
    for slot in entered_slots {
        if slot == this_slot {
            continue;
        }
        if let Some(idx) = mon_idx_for_active_slot(state, slot)
            && let Some(mon) = get_mon_by_idx(state, idx) {
                for ab in abilities {
                    if !unknown_is_excluded(&mon.possible_abilities, ab) {
                        return false;
                    }
                }
            }
    }
    true
}

fn only_slot_with_weather_setter(
    state: &UnknownBattleState,
    entered_slots: &[FieldSlot],
    this_slot: &FieldSlot,
) -> bool {
    only_slot_with_abilities(state, entered_slots, this_slot, WEATHER_SETTING_ABILITIES)
}

fn only_slot_with_terrain_setter(
    state: &UnknownBattleState,
    entered_slots: &[FieldSlot],
    this_slot: &FieldSlot,
) -> bool {
    only_slot_with_abilities(state, entered_slots, this_slot, TERRAIN_SETTING_ABILITIES)
}

fn only_slot_with_ability(
    state: &UnknownBattleState,
    entered_slots: &[FieldSlot],
    this_slot: &FieldSlot,
    ability: &Ability,
) -> bool {
    only_slot_with_abilities(state, entered_slots, this_slot, std::slice::from_ref(ability))
}

// ── Pass 2: Item presence/absence from behaviour ──────────────────────────────

fn pass2_item_from_move(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    ctx: &BattleContext,
) {
    let EventKind::MoveUsed {
        user,
        move_used,
        targets,
    } = &event.kind
    else {
        return;
    };
    let Some(move_data) = ctx.move_dex.get(move_used) else {
        return;
    };
    let is_damaging = matches!(
        move_data.category,
        MoveCategory::Physical | MoveCategory::Special
    );

    // ── Life Orb ──────────────────────────────────────────────────────────────
    if is_damaging {
        // LO recoil only fires when the move actually deals HP damage to at least one
        // target (not when all targets miss, are immune, or are behind a Substitute).
        // If no opponent took HP damage, the absence of LO recoil is uninformative —
        // excluding LO based on it would be unsound.
        let hit_any_opponent = targets.iter().any(|t| {
            event.reactions.iter().any(|r| {
                matches!(&r.kind, EventKind::DamageDealt { target, .. } if target == t)
            })
        });

        let has_lo_recoil = event
            .reactions
            .iter()
            .any(|r| matches!(&r.kind, EventKind::DamageDealt { target, .. } if target == user));

        // Self-KO moves (Explosion, Self-Destruct, Misty Explosion, Final Gambit) and any
        // move whose own recoil/crash/drain-reversal fainted the user can suppress the LO
        // chip entirely (the sim gates LO on `!attacker_fainted`) without the absence being
        // evidence against Life Orb — the faint, not the item, explains the missing chip.
        // Pass 1 has already applied any nested `Faint{user}` by the time this runs (the
        // depth-first walk visits reactions before the parent's Pass 2/3), so `mon.fainted`
        // reflects the post-move truth.
        let user_fainted = mon_idx_for_active_slot(state, user)
            .and_then(|i| get_mon_by_idx(state, i))
            .is_some_and(|m| m.fainted);

        // S21: the sim gates the LO chip on `item_is_active` — Magic Room (field-wide)
        // or Klutz on the attacker silences the recoil even when Life Orb IS held, so
        // absence is only evidence when neither can be in play.
        let items_suppressed = state.pseudo_weathers.contains(&PseudoWeather::MagicDeluge);

        if hit_any_opponent && !user_fainted && !items_suppressed
            && let Some(user_idx) = mon_idx_for_active_slot(state, user)
                && !has_lo_recoil {
                    // S24 epoch note: these ability reads are post-move, which is
                    // sound here — a mid-move ability change (Mummy on contact)
                    // replaces Magic Guard BEFORE the LO chip fires in the sim, so
                    // "no recoil + post-move non-MG ability + Life Orb" is genuinely
                    // impossible and the exclusion below remains valid.
                    let (could_mg, could_sf, could_klutz, has_secondary) = {
                        let um = get_mon_by_idx(state, user_idx);
                        (
                            um.is_some_and(|m| {
                                !unknown_is_excluded(&m.possible_abilities, &Ability::MagicGuard)
                            }),
                            um.is_some_and(|m| {
                                !unknown_is_excluded(&m.possible_abilities, &Ability::SheerForce)
                            }),
                            um.is_some_and(|m| {
                                !unknown_is_excluded(&m.possible_abilities, &Ability::Klutz)
                            }),
                            !move_data.secondaries.is_empty(),
                        )
                    };

                    if !(could_mg || could_klutz || could_sf && has_secondary) {
                        // Definitively no Life Orb on this mon.
                        if let Some(mon) = get_mon_mut_by_idx(state, user_idx) {
                            unknown_exclude(&mut mon.item, &Item::LifeOrb, "no-lo-recoil");
                        }
                    } else {
                        // Predicate: Not(LifeOrb) ∨ MagicGuard ∨ (SheerForce ∧ secondary) ∨ Klutz
                        let mut clause = vec![Statement::Not(Box::new(Statement::HasItem {
                            mon_idx: user_idx,
                            item: Item::LifeOrb,
                        }))];
                        if could_mg {
                            clause.push(Statement::HasAbility {
                                mon_idx: user_idx,
                                ability: Ability::MagicGuard,
                            });
                        }
                        if could_sf && has_secondary {
                            clause.push(Statement::HasAbility {
                                mon_idx: user_idx,
                                ability: Ability::SheerForce,
                            });
                        }
                        // S21: Klutz silences the LO chip while the orb stays held.
                        if could_klutz {
                            clause.push(Statement::HasAbility {
                                mon_idx: user_idx,
                                ability: Ability::Klutz,
                            });
                        }
                        state.predicates.push(clause);
                    }
                } // end hit_any_opponent
    } // end is_damaging

    // ── Bright Powder / Lax Incense from 100%-accurate miss ───────────────────
    for reaction in &event.reactions {
        if let EventKind::Missed { target } = &reaction.kind
            && matches!(move_data.accuracy, AccuracyType::Percent(100)) {
                // No stat-stage accuracy/evasion modifiers in play?
                let user_acc_stage = mon_idx_for_active_slot(state, user)
                    .and_then(|ui| get_mon_by_idx(state, ui))
                    .map(|m| m.boosts[5])
                    .unwrap_or(0);
                let tgt_eva_stage = mon_idx_for_active_slot(state, target)
                    .and_then(|ti| get_mon_by_idx(state, ti))
                    .map(|m| m.boosts[6])
                    .unwrap_or(0);

                if user_acc_stage >= 0 && tgt_eva_stage <= 0
                    && let Some(tgt_idx) = mon_idx_for_active_slot(state, target) {
                        let legal_ok = |item: &Item| ctx.config.legal_item_ok(item);
                        let mut clause = Vec::new();
                        if legal_ok(&Item::BrightPowder) {
                            clause.push(Statement::HasItem {
                                mon_idx: tgt_idx,
                                item: Item::BrightPowder,
                            });
                        }
                        if legal_ok(&Item::LaxIncense) {
                            clause.push(Statement::HasItem {
                                mon_idx: tgt_idx,
                                item: Item::LaxIncense,
                            });
                        }
                        // Soundness: a 100%-accurate move can also miss due to evasion
                        // *abilities* whose activating condition is currently met.  Each
                        // possible such ability must appear as a disjunct so the clause
                        // does not falsely exclude the true world.
                        //
                        // Omitting a disjunct is NARROWER (not wider) — it would force
                        // the item to be BrightPowder/LaxIncense even when a Sand-Veil
                        // mon in sandstorm caused the miss, leading to either a wrong
                        // item deduction or an inference_contradiction! panic.
                        //
                        // We snapshot the relevant target state before borrowing state
                        // mutably (via predicates.push).
                        let (tgt_abilities, tgt_has_confusion) = {
                            let tm = get_mon_by_idx(state, tgt_idx);
                            let abilities = tm.map(|m| m.possible_abilities.clone());
                            let has_confusion = tm.is_some_and(|m| {
                                m.volatiles.iter().any(|v| {
                                    matches!(
                                        v,
                                        VolatileStatusState::TurnStatus(
                                            VolatileStatus::Confusion, _
                                        ) | VolatileStatusState::MoveStatus(
                                            VolatileStatus::Confusion, _
                                        )
                                    )
                                })
                            });
                            (abilities, has_confusion)
                        };
                        let ability_not_excluded = |ab: &Ability| {
                            tgt_abilities
                                .as_ref()
                                .is_none_or(|u| !unknown_is_excluded(u, ab))
                        };
                        // Sand Veil: active under Sandstorm.
                        if matches!(state.weather, Some(Weather::Sandstorm))
                            && ability_not_excluded(&Ability::SandVeil)
                        {
                            clause.push(Statement::HasAbility {
                                mon_idx: tgt_idx,
                                ability: Ability::SandVeil,
                            });
                        }
                        // Snow Cloak: active under Snow (Gen IX "Snow" covers both
                        // classic Snow and Hail weather).
                        if matches!(state.weather, Some(Weather::Snow))
                            && ability_not_excluded(&Ability::SnowCloak)
                        {
                            clause.push(Statement::HasAbility {
                                mon_idx: tgt_idx,
                                ability: Ability::SnowCloak,
                            });
                        }
                        // Tangled Feet: active while the holder is confused.
                        if tgt_has_confusion && ability_not_excluded(&Ability::TangledFeet) {
                            clause.push(Statement::HasAbility {
                                mon_idx: tgt_idx,
                                ability: Ability::TangledFeet,
                            });
                        }

                        if !clause.is_empty() {
                            state.predicates.push(clause);
                        }
                    }
            }
    }

    let _ = targets; // suppress unused warning
}

// ── Pass 2: Flinch-cause attribution ─────────────────────────────────────────

/// When a Pokémon T fails to move due to flinch, deduce the flinch cause on the
/// opposing attacker if attribution is unambiguous.
///
/// **Soundness rules:**
/// - Only fires when T is on P1's side (attackers are P2 — the unknown side).
/// - Only fires when exactly one opposing attacker dealt damage to T this turn.
/// - Only fires when that attacker's move has no flinch secondary (so the move
///   alone cannot explain the flinch; the item/ability must explain it).
/// - Emits a disjunctive CNF clause `[KingsRock ∨ RazorFang ∨ Stench]` on the
///   attacker, pruned by already-excluded items/abilities.  BCP resolves it to a
///   unit fact when the other candidates are impossible.
/// - No inference is attempted from flinch *absence* (too weak: a 10–30% roll
///   failing is consistent with the holder having the item).
fn pass2_flinch_holder_from_cant(
    state: &mut UnknownBattleState,
    t_slot: &FieldSlot,
    ctx: &BattleContext,
) {
    // Only useful when T is on the observer's side (P1); attackers are then P2
    // (unknown).  If T is P2, the attacker is P1 (known) — clause is redundant.
    if t_slot.player != Player::P1 {
        return;
    }

    // Walk this turn's hits to find opposing attackers that landed damage on T.
    // We need exactly one unique attacker for unambiguous attribution.
    let mut attacker_slot: Option<&FieldSlot> = None;
    let mut move_used: Option<&PokemonMove> = None;
    let mut ambiguous = false;

    for (a, t, m) in &ctx.damaging_hits_this_turn {
        if t == t_slot && a.player != t_slot.player {
            match attacker_slot {
                None => {
                    attacker_slot = Some(a);
                    move_used = Some(m);
                }
                Some(existing) if existing == a => {
                    // Same attacker, different hit (e.g. multi-hit move recorded twice for
                    // different targets — shouldn't happen after dedup, but safe).
                }
                Some(_) => {
                    // Two or more distinct attackers hit T; attribution is ambiguous.
                    ambiguous = true;
                    break;
                }
            }
        }
    }

    if ambiguous || attacker_slot.is_none() {
        // Either ambiguous (multiple attackers) or no damaging hit recorded this turn.
        // In the latter case the flinch came from a move secondary, which is already
        // explained by the move; no item/ability deduction is sound.
        return;
    }

    let attacker_slot = attacker_slot.unwrap();
    let move_used = move_used.unwrap();

    // Move-secondary gate: if the move already has a flinch secondary, the flinch is
    // fully explained by the move itself — no item or ability deduction.
    let Some(move_data) = ctx.move_dex.get(move_used) else {
        return;
    };
    let move_already_flinches = move_data.secondaries.iter().any(|sec| {
        sec.effect.volatile_status == Some(VolatileStatus::Flinch)
            || sec
                .random_choices
                .iter()
                .any(|c| c.volatile_status == Some(VolatileStatus::Flinch))
    });
    if move_already_flinches {
        return;
    }

    // Resolve the attacker's mon_idx and build the disjunctive clause.
    let Some(ai) = mon_idx_for_active_slot(state, attacker_slot) else {
        return;
    };

    let legal_ok = |item: &Item| ctx.config.legal_item_ok(item);

    let mut clause: Vec<Statement> = Vec::new();
    if let Some(mon) = get_mon_by_idx(state, ai) {
        // King's Rock
        if legal_ok(&Item::KingsRock) && !unknown_is_excluded(&mon.item, &Item::KingsRock) {
            clause.push(Statement::HasItem {
                mon_idx: ai,
                item: Item::KingsRock,
            });
        }
        // Razor Fang
        if legal_ok(&Item::RazorFang) && !unknown_is_excluded(&mon.item, &Item::RazorFang) {
            clause.push(Statement::HasItem {
                mon_idx: ai,
                item: Item::RazorFang,
            });
        }
        // Stench ability (no legal_abilities gate — species possibility is the bound)
        if !unknown_is_excluded(&mon.possible_abilities, &Ability::Stench) {
            clause.push(Statement::HasAbility {
                mon_idx: ai,
                ability: Ability::Stench,
            });
        }
    }

    // Push only if at least one candidate survives pruning.
    // An empty clause would be an unsatisfiable constraint (flinch impossible under the
    // current model), which indicates a model error — skip rather than panic here.
    if !clause.is_empty() {
        state.predicates.push(clause);
    }
}

// ── Pass 2b: Contact-reaction absence inference ───────────────────────────────

/// Infer the *absence* of always-on contact-reactive items/abilities on the
/// defender when a contact move hit but produced no such reaction in its nested
/// event tree.
///
/// **Rocky Helmet** (1/6 chip to attacker) and **Rough Skin / Iron Barbs** (1/8
/// chip to attacker) are unconditional on contact — they always produce an
/// `ItemRevealed` / `AbilityRevealed` nested under the `DamageDealt` reaction.
/// If no such reveal appeared and no attacker-side escape is possible, we can
/// definitively exclude those from the defender.
///
/// Presence of these items/abilities is handled by the nested-reveal convention
/// (Pass 1 `ItemRevealed`/`AbilityRevealed`) and needs no inference here.
fn pass2_contact_absence(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    ctx: &BattleContext,
) {
    let EventKind::MoveUsed { user, targets, move_used } = &event.kind else {
        return;
    };
    let Some(move_data) = ctx.move_dex.get(move_used) else {
        return;
    };

    // Only contact moves trigger contact reactions.
    if !move_has_flag(move_data, &MoveFlag::Contact) {
        return;
    }

    // --- Attacker-side escape checks (if any apply, skip — sound: wider) ---
    let attacker_idx = mon_idx_for_active_slot(state, user);
    let attacker_escapes = {
        let am = attacker_idx.and_then(|i| get_mon_by_idx(state, i));
        // Long Reach (attacker ability) makes the move non-contact.
        let might_be_long_reach = am.is_some_and(|m| {
            !unknown_is_excluded(&m.possible_abilities, &Ability::LongReach)
        });
        // Protective Pads (attacker item) prevents contact-triggered effects.
        let might_have_pads = am.is_some_and(|m| {
            !unknown_is_excluded(&m.item, &Item::ProtectivePads)
        });
        // Magic Guard on the attacker prevents ALL three contact chips (Rough Skin,
        // Iron Barbs, Rocky Helmet — indirect damage). Gates every exclusion below.
        let might_have_magic_guard = am.is_some_and(|m| {
            !unknown_is_excluded(&m.possible_abilities, &Ability::MagicGuard)
        });
        (might_be_long_reach, might_have_pads, might_have_magic_guard)
    };
    let (long_reach_possible, pads_possible, magic_guard_possible) = attacker_escapes;

    // If either Long Reach or Protective Pads is possible, no contact reaction is
    // guaranteed — skip all exclusions (sound).
    if long_reach_possible || pads_possible {
        return;
    }

    for target in targets {
        let Some(target_idx) = mon_idx_for_active_slot(state, target) else {
            continue;
        };

        // A missed/blocked move fires no DamageDealt reaction, so this doubles as the hit check.
        let hit_landed = event.reactions.iter().any(|r| {
            matches!(&r.kind, EventKind::DamageDealt { target: t, .. } if t == target)
        });
        if !hit_landed {
            continue;
        }

        // Reveal may be nested under DamageDealt or appear directly in reactions.
        let helmet_revealed = reaction_contains_item_reveal(event, target, &Item::RockyHelmet);
        let rough_skin_revealed =
            reaction_contains_ability_reveal(event, target, &Ability::RoughSkin);
        let iron_barbs_revealed =
            reaction_contains_ability_reveal(event, target, &Ability::IronBarbs);

        // Defender-side suppression: if the DEFENDER's ability might be suppressed
        // (Neutralizing Gas possibly on the field, or Gastro Acid on the defender),
        // Rough Skin / Iron Barbs would be silent even if present — excluding them
        // would be unsound. Rocky Helmet is an item and is unaffected by ability
        // suppression, but IS silenced by item suppression (see below).
        let defender_maybe_suppressed = unknown_ability_might_be_suppressed(state, target);

        // S21: the sim gates the Helmet chip on `item_is_active` — Magic Room
        // (field-wide) or Klutz on the DEFENDER keeps the Helmet silent while it is
        // genuinely held, so absence is only evidence when neither can be in play.
        let items_suppressed = state.pseudo_weathers.contains(&PseudoWeather::MagicDeluge);

        let Some(mon) = get_mon_mut_by_idx(state, target_idx) else {
            continue;
        };
        let defender_klutz_possible =
            !unknown_is_excluded(&mon.possible_abilities, &Ability::Klutz);

        // S24 epoch note: `mon.item` here is post-move — if the observed hit consumed
        // the defender's berry, the item is already `Known(None)` and the exclusion
        // below no-ops (unknown_exclude on a different Known value). Vacuous but
        // sound; the mon provably wasn't holding a Helmet this move either way.
        //
        // Rocky Helmet: Magic Guard on the attacker prevents the chip, so Helmet
        // absence is only certain when Magic Guard is also excluded — and (S21) when
        // the Helmet itself cannot have been inert (Magic Room / defender Klutz).
        if !helmet_revealed && !magic_guard_possible && !items_suppressed && !defender_klutz_possible
        {
            unknown_exclude(&mut mon.item, &Item::RockyHelmet, "no-helmet-chip");
        }

        // Rough Skin / Iron Barbs: Magic Guard on the attacker ALSO prevents these —
        // all three contact chips (Rough Skin, Iron Barbs, Rocky Helmet) are classified
        // as "indirect damage" and are blocked by Magic Guard.  The previous comment
        // claiming Magic Guard did not apply to Rough Skin / Iron Barbs was incorrect
        // (confirmed on Bulbapedia). Gate the same way as Rocky Helmet — plus the
        // defender-suppression gate (abilities only; the Helmet is exempt).
        if !rough_skin_revealed && !magic_guard_possible && !defender_maybe_suppressed {
            unknown_exclude(
                &mut mon.possible_abilities,
                &Ability::RoughSkin,
                "no-rough-skin-chip",
            );
        }
        if !iron_barbs_revealed && !magic_guard_possible && !defender_maybe_suppressed {
            unknown_exclude(
                &mut mon.possible_abilities,
                &Ability::IronBarbs,
                "no-iron-barbs-chip",
            );
        }
    }
}

/// Recursively scan `event` and all nested reactions for any event satisfying `pred`.
fn reaction_contains<F: Fn(&EventKind) -> bool>(event: &InformationEvent, pred: &F) -> bool {
    event.reactions.iter().any(|r| pred(&r.kind) || reaction_contains(r, pred))
}

/// Like [`reaction_contains`], but starting from a reaction slice (checks each node's
/// own kind, then recurses). Used where the caller holds `&[InformationEvent]` rather
/// than the parent event.
fn any_reaction_deep<F: Fn(&EventKind) -> bool>(reactions: &[InformationEvent], pred: &F) -> bool {
    reactions.iter().any(|r| pred(&r.kind) || any_reaction_deep(&r.reactions, pred))
}

/// `true` if a nested `ItemRevealed` names `item` on `slot` anywhere under `event`.
fn reaction_contains_item_reveal(
    event: &InformationEvent,
    slot: &FieldSlot,
    item: &Item,
) -> bool {
    reaction_contains(event, &|k| {
        matches!(k, EventKind::ItemRevealed { slot: s, item: i } if s == slot && i == item)
    })
}

fn reaction_contains_ability_reveal(
    event: &InformationEvent,
    slot: &FieldSlot,
    ability: &Ability,
) -> bool {
    reaction_contains(event, &|k| {
        matches!(k, EventKind::AbilityRevealed { slot: s, ability: a } if s == slot && a == ability)
    })
}

/// Returns `true` if `event`'s reactions include any of `Immune`, `MoveFailed`, or
/// `Blocked` directed at `target`.  This covers the three ways a move can "not happen"
/// on a particular target without having missed (missed = wrong target choice, not
/// absorbed/immunity/blocked here).
///
/// Used by `pass2_prankster_immunity`, `pass2_powder_immunity`, and
/// `pass2_guaranteed_status_absence` (which also checks `Missed`).
fn move_blocked_on_target(event: &InformationEvent, target: &FieldSlot) -> bool {
    event.reactions.iter().any(|r| {
        matches!(
            &r.kind,
            EventKind::Immune { target: t }
            | EventKind::MoveFailed { slot: t }
            | EventKind::Blocked { target: t }
            if t == target
        )
    })
}

/// Returns `true` if a damaging move's reactions include `DamageDealt` for `target`
/// (confirming the hit landed), or if a status move's reactions contain none of the
/// failure signals (`Missed`, `Immune`, `MoveFailed`, `Blocked`) for `target`.
///
/// `is_damaging` selects which check to apply.
fn move_hit_target(event: &InformationEvent, target: &FieldSlot, is_damaging: bool) -> bool {
    if is_damaging {
        event.reactions.iter().any(|r| {
            matches!(&r.kind, EventKind::DamageDealt { target: t, .. } if t == target)
        })
    } else {
        !event.reactions.iter().any(|r| {
            matches!(
                &r.kind,
                EventKind::Missed { target: t }
                | EventKind::Immune { target: t }
                | EventKind::MoveFailed { slot: t }
                | EventKind::Blocked { target: t }
                if t == target
            )
        })
    }
}

// ── Pass 2c: Prankster-immunity reveal ────────────────────────────────────────

/// If the opponent used a **Status-category** move that produced an `Immune`
/// reaction on a Dark-type target, and no alternative immunity explanation is
/// possible, the Dark-type immunity to Prankster-boosted moves (Gen VII+) is the
/// only cause.  Emit `[HasAbility(Prankster)]` on the user; BCP forces `Known`.
///
/// Guards (skip when any alternative could explain the block — sound: wider):
/// - only `Immune` is accepted; `MoveFailed`/`Blocked` cover Protect, Substitute,
///   terrain, already-statused, and Dazzling-class blocks, none of which prove
///   Prankster;
/// - the target's own typing must not grant move-type immunity (e.g. Thunder
///   Wave vs a Dark/Ground target);
/// - powder moves are excluded (Grass typing / Safety Goggles / Overcoat);
/// - a nested `AbilityRevealed` on the target explains the immunity by itself;
/// - the target's ability must be Known and not immunity-granting for this move
///   type (`ability_grants_move_immunity`) — an unknown ability could be an
///   absorb ability or Good as Gold.
fn pass2_prankster_immunity(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    ctx: &BattleContext,
) {
    let EventKind::MoveUsed { user, targets, move_used } = &event.kind else {
        return;
    };
    let Some(move_data) = ctx.move_dex.get(move_used) else {
        return;
    };
    // Only Status-category moves get the Prankster +1.
    if move_data.category != MoveCategory::Status {
        return;
    }

    let Some(user_idx) = mon_idx_for_active_slot(state, user) else {
        return;
    };
    let user_mon = get_mon_by_idx(state, user_idx);

    // Already know the ability — no need to infer.
    if user_mon.is_some_and(|m| matches!(&m.possible_abilities, Unknown::Known(_))) {
        return;
    }
    // Prankster already excluded — no clause needed.
    if user_mon.is_some_and(|m| {
        unknown_is_excluded(&m.possible_abilities, &Ability::Prankster)
    }) {
        return;
    }

    // Powder moves have their own immunity web (Grass typing, Safety Goggles,
    // Overcoat) — never Prankster evidence.
    if move_has_flag(move_data, &MoveFlag::Powder) {
        return;
    }

    for target in targets {
        // Only the `Immune` reaction matches the Dark bounce ("It doesn't affect…").
        let immune = event.reactions.iter().any(|r| {
            matches!(&r.kind, EventKind::Immune { target: t } if t == target)
        });
        if !immune {
            continue;
        }

        // A nested ability reveal on the target (absorb abilities etc.) explains the
        // immunity without Prankster.
        if reaction_contains(event, &|k| {
            matches!(k, EventKind::AbilityRevealed { slot, .. } if slot == target)
        }) {
            continue;
        }

        let Some(tm) = mon_idx_for_active_slot(state, target)
            .and_then(|i| get_mon_by_idx(state, i))
        else {
            continue;
        };
        // Types must be fully Known and include Dark.
        let Unknown::Known(types) = &tm.possible_types else {
            continue;
        };
        if !types.contains(&PokemonType::Dark) {
            continue;
        }
        // Move-type immunity from the target's own typing is an alternative cause.
        if types
            .iter()
            .any(|t| single_type_effectiveness(&move_data.pokemon_type, t) == 0.0)
        {
            continue;
        }
        // The target's ability must be Known and non-immunity-granting; an unknown
        // ability could itself explain the Immune.
        match &tm.possible_abilities {
            Unknown::Known(ab) if !ability_grants_move_immunity(ab, &move_data.pokemon_type) => {}
            _ => continue,
        }

        // Emit a unit clause (or near-unit after BCP) — Prankster is the only explanation.
        state.predicates.push(vec![Statement::HasAbility {
            mon_idx: user_idx,
            ability: Ability::Prankster,
        }]);
        // Only need to emit once per user (the clause is user-specific).
        return;
    }
}

/// Abilities that can produce an `Immune` on their own for a move of the given type
/// (absorb / draw-in / blanket status immunity). Conservative superset: listing an
/// extra ability only costs completeness, never soundness.
fn ability_grants_move_immunity(ability: &Ability, move_type: &PokemonType) -> bool {
    use PokemonType::*;
    matches!(
        (move_type, ability),
        (Electric, Ability::VoltAbsorb | Ability::MotorDrive | Ability::LightningRod)
            | (Water, Ability::WaterAbsorb | Ability::StormDrain | Ability::DrySkin)
            | (Grass, Ability::SapSipper)
            | (Fire, Ability::FlashFire | Ability::WellBakedBody)
            | (Ground, Ability::EarthEater | Ability::Levitate)
            | (_, Ability::GoodasGold)
    )
}

// ── Pass 2d: Powder-move immunity reveal ──────────────────────────────────────

/// When a move with `MoveFlag::Powder` targets a **non-Grass** Pokémon and
/// results in `Immune`/`MoveFailed`/`Blocked`, the only non-type-immunity
/// explanation is Safety Goggles or Overcoat on the target.
fn pass2_powder_immunity(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    ctx: &BattleContext,
) {
    let EventKind::MoveUsed { targets, move_used, .. } = &event.kind else {
        return;
    };
    let Some(move_data) = ctx.move_dex.get(move_used) else {
        return;
    };
    if !move_has_flag(move_data, &MoveFlag::Powder) {
        return;
    }

    for target in targets {
        let Some(target_idx) = mon_idx_for_active_slot(state, target) else {
            continue;
        };
        let target_mon = get_mon_by_idx(state, target_idx);

        // Grass types are inherently immune — no item/ability inference.
        let is_grass = target_mon
            .map(|m| matches!(&m.possible_types, Unknown::Known(ts) if ts.contains(&PokemonType::Grass)))
            .unwrap_or(false);
        if is_grass {
            continue;
        }

        if !move_blocked_on_target(event, target) {
            continue;
        }

        let tm = get_mon_by_idx(state, target_idx);
        let mut clause: Vec<Statement> = Vec::new();
        let legal_ok = |item: &Item| ctx.config.legal_item_ok(item);
        if legal_ok(&Item::SafetyGoggles)
            && tm.is_none_or(|m| !unknown_is_excluded(&m.item, &Item::SafetyGoggles))
        {
            clause.push(Statement::HasItem { mon_idx: target_idx, item: Item::SafetyGoggles });
        }
        if tm.is_none_or(|m| !unknown_is_excluded(&m.possible_abilities, &Ability::Overcoat)) {
            clause.push(Statement::HasAbility { mon_idx: target_idx, ability: Ability::Overcoat });
        }
        if !clause.is_empty() {
            state.predicates.push(clause);
        }
    }
}

// ── Pass 2e: Guaranteed-status absence reveals ────────────────────────────────

/// When a move **hit** the target (a `DamageDealt` or a Status-category move with
/// no `Missed` reaction) and carries a **guaranteed status** (`chance == 100`,
/// `effect.status == Some(s)`, empty `random_choices`) yet produces **no**
/// `StatusInflicted{target}`, emit a disjunction of the unknown status-prevention
/// abilities/items on the target.
///
/// Only fires when all *decidable* preventers have been ruled out (type immunity,
/// already-statused, Substitute, Safeguard, terrain).
fn pass2_guaranteed_status_absence(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    ctx: &BattleContext,
) {
    let EventKind::MoveUsed { user: _, targets, move_used } = &event.kind else {
        return;
    };
    let Some(move_data) = ctx.move_dex.get(move_used) else {
        return;
    };

    // Find all guaranteed statuses in this move's secondaries.
    // A "guaranteed status secondary" has chance==100, one status, and no random choices.
    let is_damaging = matches!(move_data.category, MoveCategory::Physical | MoveCategory::Special);
    let guaranteed_statuses: Vec<(Status, bool)> = move_data
        .secondaries
        .iter()
        .filter(|s| {
            s.chance == 100
                && s.effect.status.is_some()
                && s.random_choices.is_empty()
        })
        .map(|s| (s.effect.status.clone().unwrap(), is_damaging))
        .collect();

    if guaranteed_statuses.is_empty() {
        return;
    }

    for target in targets {
        let Some(target_idx) = mon_idx_for_active_slot(state, target) else {
            continue;
        };

        // Did the move actually hit? (Missed / Blocked / Immune = no status applies.)
        if !move_hit_target(event, target, is_damaging) {
            continue;
        }

        // A target KO'd by this very hit receives no secondary status — the absence
        // is fully explained by the faint; emitting a preventer clause would be
        // unsound (e.g. Nuzzle KO). Pass 1 has already applied the Faint /
        // DamageDealt-to-0 reactions by the time this post-order pass runs.
        let target_fainted = get_mon_by_idx(state, target_idx).is_none_or(|m| {
            m.fainted || matches!(m.hp, PokemonHP::Number(0) | PokemonHP::Percent(0))
        });
        if target_fainted {
            continue;
        }

        let status_inflicted = event.reactions.iter().any(|r| {
            matches!(&r.kind, EventKind::StatusInflicted { target: t, .. } if t == target)
        });
        if status_inflicted {
            continue; // Status did land — nothing to infer.
        }

        // Extract all data into owned copies to avoid a live immutable borrow of state
        // when we later push to state.predicates.
        let (tm_item, tm_abilities, known_types) = snapshot_item_ability_type(state, target_idx);
        let (already_statused, has_sub) = {
            let tm = get_mon_by_idx(state, target_idx);
            let already_statused = tm.is_some_and(|m| m.status.is_some());
            let has_sub = tm.is_some_and(|m| {
                m.volatiles.iter().any(|v| matches!(
                    v,
                    VolatileStatusState::TurnStatus(VolatileStatus::Substitute(_), _)
                    | VolatileStatusState::MoveStatus(VolatileStatus::Substitute(_), _)
                ))
            });
            (already_statused, has_sub)
        };

        // Already statused prevents the secondary from applying.
        if already_statused {
            continue;
        }

        if has_sub {
            continue;
        }

        // SafeGuard on the target's side?
        let has_safeguard = {
            let is_p2 = mon_is_p2(state, target_idx);
            if is_p2 {
                state.p2_side_conditions.contains(&SideCondition::SafeGuard)
            } else {
                state.p1_side_conditions.contains(&SideCondition::SafeGuard)
            }
        };
        if has_safeguard {
            continue;
        }

        // LeafGuard only prevents status under harsh sun — snapshot weather now.
        let is_sun = matches!(state.weather, Some(Weather::Sun) | Some(Weather::ExtremeSunlight));

        let mut pending_clauses: Vec<Vec<Statement>> = Vec::new();

        for (status, from_secondary) in &guaranteed_statuses {
            // Type-immune check (known types only — absent knowledge is not immunity).
            let type_immune = match status {
                Status::Burn => known_types.as_ref().is_some_and(|ts| ts.contains(&PokemonType::Fire)),
                Status::Paralysis => known_types.as_ref().is_some_and(|ts| {
                    // Electric-type: unconditional paralysis immunity (Gen VI+).
                    // Ground-type: only immune to Electric-type paralysis moves (Thunder Wave,
                    // Nuzzle, Zap Cannon); Body Slam etc. CAN paralyze Ground-types.
                    ts.contains(&PokemonType::Electric)
                        || (ts.contains(&PokemonType::Ground)
                            && move_data.pokemon_type == PokemonType::Electric)
                }),
                Status::Poison | Status::ToxicPoison(_) => known_types.as_ref().is_some_and(|ts| {
                    ts.contains(&PokemonType::Poison) || ts.contains(&PokemonType::Steel)
                }),
                Status::Frozen(_) => known_types.as_ref().is_some_and(|ts| ts.contains(&PokemonType::Ice)),
                Status::Sleep(_) => false, // No blanket type immunity to sleep
            };
            if type_immune {
                continue;
            }

            // Terrain immunity (treat all mons as grounded for sound approximation).
            // Misty Terrain: mons immune to all status.
            // Electric Terrain: mons immune to sleep.
            let terrain_immune = match status {
                Status::Sleep(_) => state.terrain == Some(Terrain::MistyTerrain)
                    || state.terrain == Some(Terrain::ElectricTerrain),
                _ => state.terrain == Some(Terrain::MistyTerrain),
            };
            if terrain_immune {
                continue;
            }

            // Freeze in harsh sunlight: blanket immunity regardless of ability or type.
            // "Pokémon cannot be frozen when harsh sunlight is active." — Bulbapedia.
            // The absence is fully explained by weather, so emitting an ability clause
            // would be unsound (it could force-exclude a valid item/ability config).
            if matches!(status, Status::Frozen(_)) && is_sun {
                continue;
            }

            let mut clause: Vec<Statement> = Vec::new();

            // Covert Cloak: blocks secondary effects of damaging moves.
            let legal_ok = |item: &Item| ctx.config.legal_item_ok(item);
            let item_excluded_cc = tm_item.as_ref()
                .is_some_and(|it| unknown_is_excluded(it, &Item::CovertCloak));
            if *from_secondary && legal_ok(&Item::CovertCloak) && !item_excluded_cc {
                clause.push(Statement::HasItem { mon_idx: target_idx, item: Item::CovertCloak });
            }

            // Shield Dust: blocks the additional effects of damaging moves (same scope as
            // Covert Cloak — secondary effects of damaging moves only).  Shield Dust is
            // an Ignorable ability (Mold Breaker bypasses), but including it as a disjunct
            // is sound regardless.
            if *from_secondary {
                let sd_excluded = tm_abilities.as_ref()
                    .is_some_and(|pa| unknown_is_excluded(pa, &Ability::ShieldDust));
                if !sd_excluded {
                    clause.push(Statement::HasAbility {
                        mon_idx: target_idx,
                        ability: Ability::ShieldDust,
                    });
                }
            }

            // Per-status prevention abilities.
            let preventer_abilities: Vec<Ability> = match status {
                Status::Burn => vec![
                    Ability::WaterVeil,
                    Ability::WaterBubble,
                    Ability::ThermalExchange,
                    Ability::Comatose,
                    Ability::PurifyingSalt,
                    Ability::ShieldsDown,
                    Ability::LeafGuard,
                    Ability::FlowerVeil,
                ],
                Status::Paralysis => vec![
                    Ability::Limber,
                    Ability::Comatose,
                    Ability::PurifyingSalt,
                    Ability::ShieldsDown,
                    Ability::LeafGuard,
                    Ability::FlowerVeil,
                ],
                Status::Poison | Status::ToxicPoison(_) => vec![
                    Ability::Immunity,
                    Ability::PastelVeil,
                    Ability::Comatose,
                    Ability::PurifyingSalt,
                    Ability::ShieldsDown,
                    Ability::LeafGuard, // all non-volatile statuses under harsh sun
                    Ability::FlowerVeil,
                ],
                Status::Sleep(_) => vec![
                    Ability::Insomnia,
                    Ability::VitalSpirit,
                    Ability::SweetVeil,
                    Ability::Comatose,
                    Ability::PurifyingSalt,
                    Ability::ShieldsDown,
                    Ability::LeafGuard,
                    Ability::FlowerVeil,
                ],
                Status::Frozen(_) => vec![
                    Ability::MagmaArmor,
                    Ability::Comatose,
                    Ability::PurifyingSalt,
                    Ability::ShieldsDown,
                    Ability::LeafGuard, // all non-volatile statuses under harsh sun
                    Ability::FlowerVeil, // all non-volatile statuses for Grass-type holders
                ],
            };

            for ab in &preventer_abilities {
                if *ab == Ability::LeafGuard && !is_sun {
                    continue;
                }
                let ab_excluded = tm_abilities.as_ref()
                    .is_some_and(|pa| unknown_is_excluded(pa, ab));
                if !ab_excluded {
                    clause.push(Statement::HasAbility { mon_idx: target_idx, ability: ab.clone() });
                }
            }

            if !clause.is_empty() {
                pending_clauses.push(clause);
            }
        }

        for clause in pending_clauses {
            state.predicates.push(clause);
        }
    }
}

// ── Pass 2f: EOT healing reveals (Leftovers / Black Sludge) ──────────────────

/// When an opponent's Pokémon heals at end-of-turn and the cause is not
/// attributable to a known source (Aqua Ring, Ingrain, Grassy Terrain, Wish,
/// or Leech Seed draining our mon), infer Leftovers (or Leftovers ∨ Black
/// Sludge for Poison types).
fn pass_eot_heal(
    state: &mut UnknownBattleState,
    event: &InformationEvent, // must be EndOfTurn
    ctx: &BattleContext,
) {
    // Collect (target_idx, target FieldSlot) for all opponent heals in top-level reactions.
    // Gather data into owned values first to avoid holding state borrows during push.
    let legal_ok = |item: &Item| ctx.config.legal_item_ok(item);

    // If our own active mon has LeechSeed, the opponent may be getting a Leech Seed heal.
    // This is a conservative skip — if uncertain, don't infer Leftovers.
    let our_mon_is_seeded = state.p1_active_mons.iter().any(|m| {
        m.volatiles.iter().any(|v| {
            matches!(v, VolatileStatusState::TurnStatus(VolatileStatus::LeechSeed, _))
        })
    });

    let is_grassy = state.terrain == Some(Terrain::GrassyTerrain);

    let mut pending_clauses: Vec<Vec<Statement>> = Vec::new();

    for reaction in &event.reactions {
        let EventKind::Healed { target, .. } = &reaction.kind else {
            continue;
        };
        // Only infer from opponent heals (p2 from our perspective).
        if target.player != crate::state::battle::Player::P2 {
            continue;
        }

        let Some(target_idx) = mon_idx_for_active_slot(state, target) else {
            continue;
        };

        // Extract needed state into owned copies before any mutable borrow.
        let (tm_item, tm_abilities, known_types) = snapshot_item_ability_type(state, target_idx);
        let (has_aqua_ring, has_ingrain, has_wish) = {
            let tm = get_mon_by_idx(state, target_idx);
            let has_aqua_ring = tm.is_some_and(|m| {
                m.volatiles.iter().any(|v| {
                    matches!(v, VolatileStatusState::TurnStatus(VolatileStatus::AquaRing, _))
                })
            });
            let has_ingrain = tm.is_some_and(|m| {
                m.volatiles.iter().any(|v| {
                    matches!(v, VolatileStatusState::TurnStatus(VolatileStatus::Ingrain, _))
                })
            });
            let has_wish = state
                .p2_slot_conditions
                .get(target.slot_index as usize)
                .is_some_and(|conds| {
                    conds.iter().any(|c| matches!(c, SlotCondition::Wish { .. }))
                });
            (has_aqua_ring, has_ingrain, has_wish)
        };

        // Skip if a decidable EOT-heal source explains the heal.
        if has_aqua_ring || has_ingrain || is_grassy || has_wish || our_mon_is_seeded {
            continue;
        }

        // Skip if the item is already known.
        if tm_item.as_ref().is_some_and(|it| matches!(it, Unknown::Known(_))) {
            continue;
        }

        // S6 defensive guard: `emit_eot_hp_deltas` diffs HP across a whole EOT
        // sub-phase with ONE before/after snapshot — if this mon ALSO took chip
        // damage (DamageDealt) or consumed a berry (ItemLost{consumed:true}) this
        // same EndOfTurn, the observed Healed could be the NET result of chip
        // clobbered by a pinch-berry overheal, not a passive-item heal. That netting
        // is a real (separately tracked) gap in the emission layer — until it is
        // fixed there, this pass cannot soundly distinguish "pure Leftovers heal"
        // from "sandstorm chip masked by Sitrus Berry", so skip rather than risk
        // pinning the wrong item (or panicking when the true item excludes Leftovers).
        let has_other_eot_hp_event_same_target = event.reactions.iter().any(|r| {
            matches!(&r.kind, EventKind::DamageDealt { target: t, .. } if t == target)
                || matches!(&r.kind, EventKind::ItemLost { slot: t, consumed: true, .. } if t == target)
        });
        if has_other_eot_hp_event_same_target {
            continue;
        }

        let is_poison = known_types
            .as_ref()
            .is_some_and(|ts| ts.contains(&PokemonType::Poison));

        let mut clause: Vec<Statement> = Vec::new();

        if legal_ok(&Item::Leftovers)
            && tm_item
                .as_ref()
                .is_none_or(|it| !unknown_is_excluded(it, &Item::Leftovers))
        {
            clause.push(Statement::HasItem { mon_idx: target_idx, item: Item::Leftovers });
        }
        // Black Sludge heals Poison-types at the same rate; add to the disjunction.
        if is_poison
            && legal_ok(&Item::BlackSludge)
            && tm_item
                .as_ref()
                .is_none_or(|it| !unknown_is_excluded(it, &Item::BlackSludge))
            && tm_abilities
                .as_ref()
                .is_none_or(|_| true) // BlackSludge is unconditional on Poison types
        {
            clause.push(Statement::HasItem { mon_idx: target_idx, item: Item::BlackSludge });
        }

        // Weather-based passive heals produce the same EOT `Healed` signal as Leftovers:
        // Rain Dish / Dry Skin heal in rain, Ice Body heals in snow. Widen the disjunction
        // so a weather heal is not misattributed to Leftovers (widening is always sound).
        let weather_heal_abilities: &[Ability] =
            if matches!(state.weather, Some(Weather::Rain) | Some(Weather::HeavyRain)) {
                &[Ability::RainDish, Ability::DrySkin]
            } else if matches!(state.weather, Some(Weather::Snow)) {
                &[Ability::IceBody]
            } else {
                &[]
            };
        for ab in weather_heal_abilities {
            let excluded = tm_abilities
                .as_ref()
                .is_some_and(|pa| unknown_is_excluded(pa, ab));
            if !excluded {
                clause.push(Statement::HasAbility { mon_idx: target_idx, ability: ab.clone() });
            }
        }

        if !clause.is_empty() {
            pending_clauses.push(clause);
        }
    }

    for clause in pending_clauses {
        state.predicates.push(clause);
    }
}

// ── Pass 2g: Sandstorm EOT chip absence → immunity reveal ─────────────────────

/// When Sandstorm is active and an opponent's non-Rock/Ground/Steel Pokémon takes
/// **no** EOT sand chip, emit a disjunction of the abilities/items that grant
/// sand immunity.
fn pass_eot_sand_immunity(
    state: &mut UnknownBattleState,
    event: &InformationEvent, // must be EndOfTurn
    ctx: &BattleContext,
) {
    if !matches!(state.weather, Some(Weather::Sandstorm)) {
        return;
    }

    // Air Lock / Cloud Nine suspend the sandstorm chip for every mon while weather still
    // reads Sandstorm — the absence of chip then proves nothing about immunity. Skip (sound).
    if weather_effects_might_be_suspended(state) {
        return;
    }

    let legal_ok = |item: &Item| ctx.config.legal_item_ok(item);

    // p2 active mons start after all p1 segments.
    let p2_active_start = p2_mon_start(state);

    let p2_active_count = state.p2_active_mons.len();

    let mut pending_clauses: Vec<Vec<Statement>> = Vec::new();

    for slot_i in 0..p2_active_count {
        let mon_idx = p2_active_start + slot_i;
        let field_slot = FieldSlot {
            player: Player::P2,
            slot_index: slot_i as u8,
        };

        // A fainted mon takes no chip regardless of immunity — skip.
        if get_mon_by_idx(state, mon_idx).is_none_or(|m| m.fainted) {
            continue;
        }

        // Extract data into owned values to avoid borrow conflicts.
        let (tm_item, tm_abilities, known_types) = snapshot_item_ability_type(state, mon_idx);

        // Types must be known: an unknown type could be Rock/Ground/Steel (innately immune),
        // so absence of chip could be explained by typing — no item/ability inference.
        // (Mirrors the types-known guard in `pass2_ground_immune_clause`.)
        let Some(types) = known_types.as_ref() else {
            continue;
        };
        // Rock, Ground, Steel types are innately immune — no inference.
        if types.contains(&PokemonType::Rock)
            || types.contains(&PokemonType::Ground)
            || types.contains(&PokemonType::Steel)
        {
            continue;
        }

        let took_sand_chip = event.reactions.iter().any(|r| {
            matches!(&r.kind, EventKind::DamageDealt { target: t, .. } if t == &field_slot)
        });
        if took_sand_chip {
            continue;
        }

        // S6 defensive guard: `emit_eot_hp_deltas` diffs HP across the whole weather
        // sub-phase with ONE before/after snapshot. If the sand chip fired but was
        // fully offset by a pinch berry (Sitrus etc.) triggered by that SAME chip
        // (`deal_residual_damage` calls `take_damage`, which checks berries
        // internally), the net delta can show as a `Healed` event instead of
        // `DamageDealt` — or, in the exact-cancel case, no HP-change event at all,
        // only the berry's `ItemLost`. Either way "no DamageDealt" is NOT reliable
        // evidence of immunity here; skip rather than risk an unsound sand-immunity
        // clause (or a later contradiction-panic when the true item conflicts).
        let chip_masked_by_berry = event.reactions.iter().any(|r| {
            matches!(&r.kind, EventKind::Healed { target: t, .. } if t == &field_slot)
                || matches!(&r.kind, EventKind::ItemLost { slot: t, consumed: true, .. } if t == &field_slot)
        });
        if chip_masked_by_berry {
            continue;
        }

        let mut clause: Vec<Statement> = Vec::new();

        if legal_ok(&Item::SafetyGoggles)
            && tm_item
                .as_ref()
                .is_none_or(|it| !unknown_is_excluded(it, &Item::SafetyGoggles))
        {
            clause.push(Statement::HasItem { mon_idx, item: Item::SafetyGoggles });
        }

        for ab in &[
            Ability::SandVeil,
            Ability::SandRush,
            Ability::SandForce,
            Ability::Overcoat,
            Ability::MagicGuard,
        ] {
            let excluded = tm_abilities
                .as_ref()
                .is_some_and(|pa| unknown_is_excluded(pa, ab));
            if !excluded {
                clause.push(Statement::HasAbility { mon_idx, ability: ab.clone() });
            }
        }

        if !clause.is_empty() {
            pending_clauses.push(clause);
        }
    }

    for clause in pending_clauses {
        state.predicates.push(clause);
    }
}

// ── I2: EOT self-status orb reveal ────────────────────────────────────────────

/// When a Pokémon gets a **new** non-volatile status from an `EndOfTurn` event
/// (i.e. no prior status) and the only EOT source for that status is a held item,
/// reveal the item as `Known`:
///
/// * `StatusInflicted{Burn}` at EOT → `Known(FlameOrb)` — the only held item that
///   self-inflicts Burn at end of turn.
/// * `StatusInflicted{ToxicPoison}` at EOT → `Known(ToxicOrb)` — the only held
///   item that self-inflicts bad poison at end of turn.
///
/// Soundness guards:
///   1. The mon must have had **no status** before this EOT (status set by pass1
///      during recursive descent runs after this pass, so `mon.status` is pre-EOT).
///   2. Only infer for P2 mons (opponent — P1 item is already known).
///   3. Skip if the item is already `Known` (nothing to infer).
///   4. Skip if the inferred item is already excluded from the item's possibility set.
fn pass_eot_self_status(
    state: &mut UnknownBattleState,
    event: &InformationEvent, // must be EndOfTurn
    ctx: &BattleContext,
) {
    let legal_ok = |item: &Item| ctx.config.legal_item_ok(item);

    for reaction in &event.reactions {
        let (target_slot, status) = match &reaction.kind {
            EventKind::StatusInflicted { target, status } => (target, status),
            _ => continue,
        };

        // Only infer for opponents (P2).
        if target_slot.player != crate::state::battle::Player::P2 {
            continue;
        }

        let Some(target_idx) = mon_idx_for_active_slot(state, target_slot) else {
            continue;
        };

        // Extract needed state before any mutable borrow.
        let (mon_item, had_prior_status) = {
            let mon = get_mon_by_idx(state, target_idx);
            let item = mon.map(|m| m.item.clone()).unwrap_or(Unknown::Not(Vec::new()));
            // pass1 hasn't processed this StatusInflicted yet (recursive descent runs
            // after this pass for EndOfTurn reactions), so mon.status is the pre-EOT value.
            let had_status = mon.is_some_and(|m| m.status.is_some());
            (item, had_status)
        };

        // Guard: no pre-existing status (orbs only apply when the holder is healthy).
        if had_prior_status {
            continue;
        }

        // Guard: item already known — nothing to infer.
        if matches!(mon_item, Unknown::Known(_)) {
            continue;
        }

        let infer_item = match status {
            Status::Burn => Item::FlameOrb,
            Status::ToxicPoison(_) => Item::ToxicOrb,
            _ => continue, // Paralysis, Sleep, Freeze, plain Poison not caused by orbs.
        };

        if !legal_ok(&infer_item) {
            continue;
        }
        // Guard: the inferred item is not already excluded.
        if unknown_is_excluded(&mon_item, &infer_item) {
            continue;
        }

        if let Some(mon) = get_mon_mut_by_idx(state, target_idx) {
            unknown_set_known(
                &mut mon.item,
                infer_item,
                &format!("mon#{target_idx} eot-orb"),
            );
        }
    }
}

// ── I2: Ground-type immunity clause ───────────────────────────────────────────

/// When a Ground-type damaging move results in `Immune` on a P2 mon whose types
/// are **fully known** and do not include Flying, the immunity must come from a
/// held item or ability.  Emit a disjunctive CNF clause so BCP can force the
/// exact explanation once other facts narrow the candidate set:
///
///   `HasItem(AirBalloon) ∨ HasAbility(Levitate) ∨ HasAbility(Eelevate) ∨ HasAbility(EarthEater)`
///
/// Soundness guards:
///   * Only fire when types are `Known` and exclude Flying (unknown types → could
///     be Flying → not safe to emit this clause).
///   * Skip if the mon has Magnet Rise or Telekinesis volatile (Ground immunity
///     explained by field effect — no item/ability clause needed).
///   * Exclude disjuncts already impossible (item excluded / ability excluded).
fn pass2_ground_immune_clause(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    ctx: &BattleContext,
) {
    let EventKind::MoveUsed { targets, move_used, .. } = &event.kind else {
        return;
    };
    let Some(move_data) = ctx.move_dex.get(move_used) else {
        return;
    };
    // Only Ground-type damaging moves.
    if move_data.pokemon_type != PokemonType::Ground {
        return;
    }
    if move_data.category == MoveCategory::Status {
        return;
    }

    let mut pending_clauses: Vec<Vec<Statement>> = Vec::new();

    for target in targets {
        // Only infer from opponent (P2) immunity.
        if target.player != crate::state::battle::Player::P2 {
            continue;
        }

        let immune_on_target = event.reactions.iter().any(|r| {
            matches!(&r.kind, EventKind::Immune { target: t } if t == target)
        });
        if !immune_on_target {
            continue;
        }

        let Some(target_idx) = mon_idx_for_active_slot(state, target) else {
            continue;
        };

        let (tm_item, tm_abilities, known_types) = snapshot_item_ability_type(state, target_idx);
        let (has_magnet_rise, has_telekinesis) = {
            let tm = get_mon_by_idx(state, target_idx);
            let has_magnet_rise = tm.is_some_and(|m| {
                m.volatiles.iter().any(|v| {
                    matches!(v, VolatileStatusState::TurnStatus(VolatileStatus::MagnetRise, _))
                })
            });
            let has_telekinesis = tm.is_some_and(|m| {
                m.volatiles.iter().any(|v| {
                    matches!(v, VolatileStatusState::TurnStatus(VolatileStatus::Telekinesis, _))
                })
            });
            (has_magnet_rise, has_telekinesis)
        };

        // Guard: types must be fully known.
        let Some(types) = known_types else { continue };

        // Guard: if Flying type is already in the type set, the immunity is explained.
        if types.contains(&PokemonType::Flying) {
            continue;
        }

        // Guard: Magnet Rise / Telekinesis volatiles explain the immunity.
        if has_magnet_rise || has_telekinesis {
            continue;
        }

        // Build the disjunctive clause: each candidate that is not already excluded.
        let mut clause: Vec<Statement> = Vec::new();

        // Air Balloon item.
        let ab_excluded = tm_item
            .as_ref()
            .is_some_and(|it| unknown_is_excluded(it, &Item::AirBalloon));
        if !ab_excluded {
            clause.push(Statement::HasItem { mon_idx: target_idx, item: Item::AirBalloon });
        }

        // Levitate ability.
        let lev_excluded = tm_abilities
            .as_ref()
            .is_some_and(|ab| unknown_is_excluded(ab, &Ability::Levitate));
        if !lev_excluded {
            clause.push(Statement::HasAbility { mon_idx: target_idx, ability: Ability::Levitate });
        }

        // Eelevate ability (custom ability with Levitate effect).
        let eel_excluded = tm_abilities
            .as_ref()
            .is_some_and(|ab| unknown_is_excluded(ab, &Ability::Eelevate));
        if !eel_excluded {
            clause.push(Statement::HasAbility { mon_idx: target_idx, ability: Ability::Eelevate });
        }

        // Earth Eater ability (absorbs Ground-type moves).
        let ee_excluded = tm_abilities
            .as_ref()
            .is_some_and(|ab| unknown_is_excluded(ab, &Ability::EarthEater));
        if !ee_excluded {
            clause.push(Statement::HasAbility {
                mon_idx: target_idx,
                ability: Ability::EarthEater,
            });
        }

        if !clause.is_empty() {
            pending_clauses.push(clause);
        }
    }

    for clause in pending_clauses {
        state.predicates.push(clause);
    }
}

// ── Pass 3: Damage → stat bounds ──────────────────────────────────────────────

/// Damage-to-stat inference: called once per top-level `MoveUsed` event after
/// the full reaction tree has been walked (so HP deltas and crit flags are live).
///
/// **Design**: instead of a hand-rolled analytic inverse (fragile with 22 flooring
/// steps), we use the real simulator oracle
/// `calculate_damage_outcomes_for_target_with_options` as a forward model and
/// enumerate candidate stats to find which ones can reproduce the observed damage.
///
/// **Direction B** (opponent attacks our known Pokémon): HP delta is exact
/// (`PokemonHP::Number`); we bound the attacker's Atk/SpA.
///
/// **Direction A** (we attack the opponent): HP delta is a percent interval;
/// we bound the defender's Def/SpD and HP.
///
/// **Soundness**: we always take the *union* over all possible (item, ability,
/// nature-class) assignments, so we never exclude a training that could produce
/// the observed damage.  Conditional CNF clauses let BCP recover precision as
/// other passes exclude boosters.
fn pass3_damage_to_stats(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    ctx: &BattleContext,
) {
    use crate::information::materialize::{materialize_battle, materialize_pokemon};
    use crate::simulator::helpers::calculate_damage_outcomes_for_target_with_options;
    use crate::simulator::DamageConfig;

    let EventKind::MoveUsed {
        user,
        move_used,
        targets,
    } = &event.kind
    else {
        return;
    };

    let Some(move_data) = ctx.move_dex.get(move_used) else {
        return;
    };

    // ── Skip moves that carry no stat signal ─────────────────────────────────
    // Status moves, OHKO, fixed-damage, retaliation, Super Fang, etc.
    use crate::state::dex_data::{DamageOverride, MoveCategory};
    if matches!(move_data.category, MoveCategory::Status) {
        return;
    }
    if move_data.ohko {
        return;
    }
    if !matches!(move_data.damage_override, DamageOverride::None) {
        return;
    }
    // Retaliation moves (Counter/Mirror Coat/Metal Burst/Comeuppance):
    // damage is a multiple of incoming damage, not of the user's stats.
    use crate::data::pokemon_move::PokemonMove as PM;
    if matches!(
        move_used,
        PM::Counter | PM::MirrorCoat | PM::MetalBurst | PM::Comeuppance
    ) {
        return;
    }
    // Ambiguous offensive stat (Shell Side Arm, Photon Geyser): skip in v1.
    if matches!(move_used, PM::ShellSideArm | PM::PhotonGeyser) {
        return;
    }
    // Beat Up BP depends on party members' base Attack — out of scope for inference.
    if matches!(move_used, PM::BeatUp) {
        return;
    }
    // Need a clear offensive stat to know which field to bound.
    let Some(off_stat) = crate::simulator::helpers::move_offensive_stat(move_data) else {
        return;
    };

    let Some(user_idx) = mon_idx_for_active_slot(state, user) else {
        return;
    };

    // Whether the move has variable BP determined by speed stats (Gyro Ball / Electro Ball).
    let speed_dep_bp = is_speed_dependent_bp(move_used);

    // For each target that has one or more DamageDealt reactions, run inference.
    // Multi-hit moves produce multiple DamageDealt reactions per target; we process
    // each hit independently to intersect BSV feasibility constraints.
    for target_slot in targets {
        let Some(target_idx) = mon_idx_for_active_slot(state, target_slot) else {
            continue;
        };

        // Count this target's damaging hits (multi-hit detection).
        let n_hits = event
            .reactions
            .iter()
            .filter(|r| {
                matches!(&r.kind, EventKind::DamageDealt { target, .. } if target == target_slot)
            })
            .count();
        if n_hits == 0 {
            continue;
        }

        // Pre-hit HP from MoveContext snapshot.
        let Some(move_ctx) = &ctx.move_context else {
            continue;
        };
        let pre_hp = move_ctx
            .pre_hit_hp
            .iter()
            .find(|(slot, _)| slot == target_slot)
            .map(|(_, hp)| hp);
        let Some(pre_hp) = pre_hp else {
            continue;
        };

        // Detect whether this target's HP is a multi-hit sequence.
        let is_multi = move_data.multihit_range[0] > 0 || n_hits > 1;

        // S23: walk the target's reactions IN ORDER, fixing three multi-hit bugs in
        // the old DamageDealt-only collection:
        //
        // 1. Per-hit crit — `Crit{target}` is emitted immediately before its hit's
        //    `DamageDealt`, so a pending flag attributes it to exactly that hit. The
        //    old global "any hit critted" flag also constrained non-crit hits to
        //    crit-only rolls, excluding the true (lower) BSV whenever a multi-hit
        //    mixed crits.
        //
        // 2. Interleaved heals — a mid-sequence pinch berry emits its own `Healed`
        //    between `DamageDealt`s; skipping it left the next hit's baseline
        //    pre-berry, understating that hit's damage.
        //
        // 3. `current_hp` threads each hit's true pre-hit HP into the oracle, so
        //    full-HP-gated reducers (Multiscale/Shadow Shield/Tera Shell) aren't
        //    evaluated against the already-post-move HP Pass 1 leaves on the live mon.
        let mut current_hp: PokemonHP = pre_hp.clone();
        let mut hit_idx: usize = 0;

        for reaction in &event.reactions {
            let new_hp = match &reaction.kind {
                EventKind::Healed { target, new_hp, .. } | EventKind::SetHp { target, new_hp, .. }
                    if target == target_slot =>
                {
                    // Baseline moves without being a damaging hit.
                    current_hp = new_hp.clone();
                    continue;
                }
                EventKind::DamageDealt { target, new_hp, .. } if target == target_slot => new_hp,
                _ => continue,
            };
            // S39: `Crit{target}` is emitted as a REACTION (child) of its `DamageDealt`
            // node, not a preceding sibling — see `with_reactions(bs, DamageDealt{...},
            // run_damage_reactions)` in simulator/mod.rs, which nests Crit/Faint under
            // the specific hit they belong to (the same shape a player sees: "A critical
            // hit!" attached to that hit's damage line). A `pending_crit` flag scanning
            // for a *preceding sibling* `Crit` can never fire — `event.reactions` (this
            // MoveUsed node's direct children) never contains a bare `Crit`, so `is_crit`
            // was unconditionally `false` here, silently discarding every real crit. That
            // fed pass3_direction_a/b's oracle a non-crit-only search space for hits that
            // were actually crits, producing a stat window that EXCLUDES the true value
            // (unsound) whenever the observed damage required the crit multiplier to
            // explain — the root cause of the "every candidate nature is infeasible"
            // contradictions on crit hits. Look inside this DamageDealt's own nested
            // reactions instead.
            let is_crit = reaction
                .reactions
                .iter()
                .any(|r| matches!(&r.kind, EventKind::Crit { target } if target == target_slot));

            // Per-hit BP override for fixed-BP multi-hit moves (Triple Kick etc.).
            // None for normal multi-hit moves (each hit uses move's base_power).
            let bp_override: Option<u16> = if is_multi {
                match move_used {
                    PM::TripleKick => Some(10 + hit_idx as u16 * 10),
                    PM::TripleAxel => Some(20 + hit_idx as u16 * 20),
                    PM::PopulationBomb => Some(20),
                    _ => None,
                }
            } else {
                None
            };
            hit_idx += 1;

            // ── Classify direction ────────────────────────────────────────────
            // Direction B: target HP is Number → exact damage; bound ATTACKER's stat.
            // Direction A: target HP is Percent → interval damage; bound DEFENDER's stat.
            match (&current_hp, new_hp) {
                (PokemonHP::Number(pre), PokemonHP::Number(post)) => {
                    let exact_damage = (*pre).saturating_sub(*post);
                    if exact_damage > 0 {
                        pass3_direction_b(
                            state,
                            event,
                            ctx,
                            user_idx,
                            target_idx,
                            user,
                            target_slot,
                            move_data,
                            &off_stat,
                            is_crit,
                            &current_hp,
                            exact_damage,
                            bp_override,
                            speed_dep_bp,
                        );
                    }
                }
                (PokemonHP::Percent(pre_pct), PokemonHP::Percent(post_pct)) => {
                    if *post_pct < *pre_pct {
                        let Some(def_stat) =
                            crate::simulator::helpers::move_defensive_stat(move_data)
                        else {
                            current_hp = new_hp.clone();
                            continue;
                        };
                        // S22: pass both display percents — the damage band must
                        // account for the rounding of each endpoint separately.
                        pass3_direction_a(
                            state,
                            event,
                            ctx,
                            user_idx,
                            target_idx,
                            user,
                            target_slot,
                            move_data,
                            &def_stat,
                            is_crit,
                            &current_hp,
                            *pre_pct,
                            *post_pct,
                            bp_override,
                            speed_dep_bp,
                        );
                    }
                }
                _ => {
                    // Mixed Number/Percent — HP tracking not implemented; skip.
                }
            }

            current_hp = new_hp.clone();
        }
    }
}

/// S28: `true` if the attacker at `user_slot` is this turn's last move-committed
/// actor, i.e. Analytic's ×1.3 applied. Reads the precomputed per-segment last-mover.
fn analytic_fired(ctx: &BattleContext, user_slot: &FieldSlot) -> bool {
    ctx.analytic_last_movers
        .get(ctx.turn_segment)
        .and_then(|o| o.as_ref()) == Some(user_slot)
}

/// Returns `true` for moves whose base power depends on one or both mons' Speed stats.
fn is_speed_dependent_bp(move_used: &PokemonMove) -> bool {
    matches!(move_used, PokemonMove::GyroBall | PokemonMove::ElectroBall)
}

/// Items that can boost offensive damage for a given attacker mon.
/// We enumerate these to build the booster disjuncts in CNF clauses.
pub(crate) fn offensive_damage_items(mon: &UnknownPokemonState) -> Vec<Item> {
    // Damage-relevant items (subset whose presence/absence matters for Pass 3).
    // Metronome streak is handled separately (by varying consecutive_move_count).
    [
        Item::ChoiceBand,
        Item::ChoiceSpecs,
        Item::LifeOrb,
        Item::ExpertBelt,
        Item::MuscleBand,
        Item::WiseGlasses,
        Item::Charcoal,
        Item::MysticWater,
        Item::SharpBeak,
        Item::TwistedSpoon,
        Item::BlackGlasses,
        Item::PoisonBarb,
        Item::SoftSand,
        Item::HardStone,
        Item::Magnet,
        Item::MetalCoat,
        Item::NeverMeltIce,
        Item::SilkScarf,
        Item::BlackBelt,
        Item::SpellTag,
        Item::MiracleSeed,
        Item::DragonFang,
        Item::FairyFeather,
        Item::SilverPowder, // ×1.2 to Bug-type moves
        Item::Metronome,
        Item::LightBall, // Pikachu only
    ]
    .iter()
    .filter(|i| !unknown_is_excluded(&mon.item, i))
    .cloned()
    .collect()
}

/// Abilities that can boost offensive damage.
pub(crate) fn offensive_damage_abilities(mon: &UnknownPokemonState) -> Vec<Ability> {
    [
        Ability::HugePower,
        Ability::PurePower,
        Ability::Hustle,
        Ability::Guts,
        Ability::Adaptability,
        Ability::Technician,
        Ability::ToughClaws,
        Ability::IronFist,
        Ability::Sharpness,
        Ability::MegaLauncher,
        Ability::StrongJaw,
        Ability::Reckless,
        Ability::SandForce,
        Ability::SheerForce,
        Ability::WaterBubble,
        Ability::SolarPower,
        Ability::OrichalcumPulse,
        Ability::HadronEngine,
        Ability::Rivalry,
        Ability::Blaze,
        Ability::Overgrow,
        Ability::Swarm,
        Ability::Torrent,
        Ability::FireMane,
        Ability::FlashFire,
        Ability::SupremeOverlord,
        Ability::Sniper,
        Ability::Plus,
        Ability::Minus,
        // -ate abilities
        Ability::Pixilate,
        Ability::Refrigerate,
        Ability::Aerilate,
        Ability::Galvanize,
        Ability::Normalize,
        Ability::Dragonize,
        Ability::Eelevate,
        // Analytic: ×1.3 when moving last. In oracle calls the action_queue is empty,
        // so attacker_is_last_mover always returns true → Analytic always fires.
        Ability::Analytic,
        // FairyAura: field ability, ×5448/4096 to all Fairy moves when any active mon
        // holds it. The attacker holding FairyAura boosts its own Fairy-type moves.
        Ability::FairyAura,
        // MegaSol: holder perceives Sun weather. Weakens Water moves (×0.5) and boosts
        // Fire (×1.5), so oracle output differs from baseline for those move types.
        Ability::MegaSol,
        // LiquidVoice: converts Sound-based moves to Water type, changing STAB and
        // type-effectiveness calculations and thus oracle output.
        Ability::LiquidVoice,
    ]
    .iter()
    .filter(|a| !unknown_is_excluded(&mon.possible_abilities, a))
    .cloned()
    .collect()
}

/// "No-booster" placeholders used when we want to compute the neutral bound.
fn neutral_item(mon: &UnknownPokemonState) -> Item {
    if let Unknown::Known(i) = &mon.item {
        i.clone()
    } else {
        Item::None
    }
}

fn neutral_ability(mon: &UnknownPokemonState) -> Ability {
    if let Unknown::Known(a) = &mon.possible_abilities {
        a.clone()
    } else {
        Ability::None
    }
}

// ── E-B helpers: prune provably-inert modifiers ─────────────────────────────
//
// These three functions support the E-B optimisation: given a specific move's
// category, type, and flags, they let callers drop defensive/offensive list
// entries that cannot possibly change the oracle's damage output.
//
// **Soundness contract**: each prune rule must be conservative.  When in doubt
// — e.g. the defender's types are unknown, so we can't check SE — keep the
// entry.  The S-C cross-validation test (`test_sc_allowlist_completeness_cross_
// validation`) will catch any rule that is too aggressive.

/// Infer the effective move type for pruning purposes without a full BattleState.
///
/// Handles the main type-converting attacker abilities (LiquidVoice, -ate family).
/// Ignores rare volatile-based changes (Electrify) — conservative, never prunes too much.
fn pruning_move_type(atk_ability: &Ability, move_data: &MoveData) -> PokemonType {
    if *atk_ability == Ability::LiquidVoice && move_has_flag(move_data, &MoveFlag::Sound) {
        return PokemonType::Water;
    }
    if matches!(move_data.pokemon_type, PokemonType::Normal) {
        let converted = match atk_ability {
            Ability::Aerilate    => Some(PokemonType::Flying),
            Ability::Pixilate    => Some(PokemonType::Fairy),
            Ability::Refrigerate => Some(PokemonType::Ice),
            Ability::Dragonize   => Some(PokemonType::Dragon),
            Ability::Galvanize   => Some(PokemonType::Electric),
            _ => None,
        };
        if let Some(t) = converted {
            return t;
        }
    }
    move_data.pokemon_type.clone()
}

/// Returns the type a type-resist berry absorbs, or `None` for non-berry items.
fn type_resist_berry_type(item: &Item) -> Option<PokemonType> {
    match item {
        Item::OccaBerry   => Some(PokemonType::Fire),
        Item::PasshoBerry => Some(PokemonType::Water),
        Item::WacanBerry  => Some(PokemonType::Electric),
        Item::RindoBerry  => Some(PokemonType::Grass),
        Item::YacheBerry  => Some(PokemonType::Ice),
        Item::ChopleBerry => Some(PokemonType::Fighting),
        Item::KebiaBerry  => Some(PokemonType::Poison),
        Item::ShucaBerry  => Some(PokemonType::Ground),
        Item::CobaBerry   => Some(PokemonType::Flying),
        Item::PayapaBerry => Some(PokemonType::Psychic),
        Item::TangaBerry  => Some(PokemonType::Bug),
        Item::ChartiBerry => Some(PokemonType::Rock),
        Item::KasibBerry  => Some(PokemonType::Ghost),
        Item::HabanBerry  => Some(PokemonType::Dragon),
        Item::ColburBerry => Some(PokemonType::Dark),
        Item::BabiriBerry => Some(PokemonType::Steel),
        Item::RoseliBerry => Some(PokemonType::Fairy),
        Item::ChilanBerry => Some(PokemonType::Normal),
        _ => None,
    }
}

/// Returns the type amplified by a type-specific offensive item, or `None`.
fn type_boosting_item_type(item: &Item) -> Option<PokemonType> {
    match item {
        Item::Charcoal     => Some(PokemonType::Fire),
        Item::MysticWater  => Some(PokemonType::Water),
        Item::SharpBeak    => Some(PokemonType::Flying),
        Item::TwistedSpoon => Some(PokemonType::Psychic),
        Item::BlackGlasses => Some(PokemonType::Dark),
        Item::PoisonBarb   => Some(PokemonType::Poison),
        Item::SoftSand     => Some(PokemonType::Ground),
        Item::HardStone    => Some(PokemonType::Rock),
        Item::Magnet       => Some(PokemonType::Electric),
        Item::MetalCoat    => Some(PokemonType::Steel),
        Item::NeverMeltIce => Some(PokemonType::Ice),
        Item::SilkScarf    => Some(PokemonType::Normal),
        Item::BlackBelt    => Some(PokemonType::Fighting),
        Item::SpellTag     => Some(PokemonType::Ghost),
        Item::MiracleSeed  => Some(PokemonType::Grass),
        Item::DragonFang   => Some(PokemonType::Dragon),
        Item::FairyFeather => Some(PokemonType::Fairy),
        Item::SilverPowder => Some(PokemonType::Bug),
        _ => None,
    }
}

/// Items that can reduce incoming damage for the **defender**.
///
/// Used in Direction A to union over possible defensive items when back-solving
/// the defender's defensive BSV. Without this union, the feasibility scan
/// materializes the defender with no item (neutral), which over-estimates the
/// minimum defensive BSV when the true defender has a bulk item (S1 soundness fix).
///
/// **Completeness is a soundness invariant, not an optimisation.**  If a reducer
/// item is omitted, the feasibility scan never materializes the defender with it,
/// so the "lower BSV + reducer" scenario is excluded and `min_pre_nature_stat`
/// may be raised above the true value — an unsound exclusion.
///
/// Always includes `neutral_item(mon)` (the Known item, or `Item::None`) so the
/// "no-boosting item" scenario is always in the candidate set.  Type-resist
/// berries that do not apply (wrong type / not super-effective) produce no change
/// in the oracle output, so including them is safe — the oracle gates on
/// `resist_berry_triggers`.
pub(crate) fn defensive_damage_items(mon: &UnknownPokemonState) -> Vec<Item> {
    let mut items: Vec<Item> = [
        Item::Eviolite,    // ×1.5 Def/SpDef for non-fully-evolved species
        Item::AssaultVest, // ×1.5 SpDef (special moves only)
        // Type-resist berries: each halves damage from one super-effective type.
        // Chilan Berry is Normal-type resist (any Normal hit, not just SE).
        // The damage oracle gates activation on type effectiveness, so including
        // all berries is safe for non-matching move types.
        Item::OccaBerry,   // Fire SE
        Item::PasshoBerry, // Water SE
        Item::WacanBerry,  // Electric SE
        Item::RindoBerry,  // Grass SE
        Item::YacheBerry,  // Ice SE
        Item::ChopleBerry, // Fighting SE
        Item::KebiaBerry,  // Poison SE
        Item::ShucaBerry,  // Ground SE
        Item::CobaBerry,   // Flying SE
        Item::PayapaBerry, // Psychic SE
        Item::TangaBerry,  // Bug SE
        Item::ChartiBerry, // Rock SE
        Item::KasibBerry,  // Ghost SE
        Item::HabanBerry,  // Dragon SE
        Item::ColburBerry, // Dark SE
        Item::BabiriBerry, // Steel SE
        Item::RoseliBerry, // Fairy SE
        Item::ChilanBerry, // Normal (any Normal hit)
    ]
    .iter()
    .filter(|i| !unknown_is_excluded(&mon.item, i))
    .cloned()
    .collect();
    // Always include the neutral item so the no-boost scenario is covered.
    let neutral = neutral_item(mon);
    if !items.contains(&neutral) {
        items.push(neutral);
    }
    items
}

/// Abilities that can reduce incoming damage for the **defender**.
///
/// Parallel to `offensive_damage_abilities` but for the defensive side.
/// Always includes `neutral_ability(mon)` so the no-boost scenario is covered.
///
/// **Completeness is a soundness invariant.**  Any reducer the damage oracle
/// implements but this list omits will cause `min_pre_nature_stat` to be raised
/// above the true value for defenders that could have that ability.
pub(crate) fn defensive_damage_abilities(mon: &UnknownPokemonState) -> Vec<Ability> {
    let mut abilities: Vec<Ability> = [
        Ability::Filter,       // ×0.75 on super-effective hits
        Ability::SolidRock,    // ×0.75 on super-effective hits
        Ability::PrismArmor,   // ×0.75 on super-effective hits (pierces Mold Breaker)
        Ability::Multiscale,   // ×0.5 at full HP
        Ability::ShadowShield, // ×0.5 at full HP (Lunala only)
        Ability::TeraShell,    // all moves → not-very-effective (≈×0.5) at full HP
        Ability::PurifyingSalt, // ×0.5 to Ghost-type moves
        Ability::ThickFat,     // ×0.5 to Fire and Ice moves
        Ability::FurCoat,      // ×0.5 to Physical moves
        Ability::IceScales,    // ×0.5 to Special moves
        Ability::Heatproof,    // ×0.5 to Fire moves
        Ability::Fluffy,       // ×0.5 to contact moves (but ×2 to Fire — oracle handles both)
        Ability::PunkRock,     // ×0.5 to sound-based moves received
        Ability::WaterBubble,  // ×0.5 to Fire moves received
        // FairyAura: field ability, ×5448/4096 to all Fairy moves for ALL Pokémon on
        // field. When the DEFENDER holds FairyAura, incoming Fairy moves are boosted →
        // a lower defensive BSV + FairyAura can explain the same observed Fairy damage,
        // so omitting it would raise min_pre_nature_stat unsoundly.
        Ability::FairyAura,
        // DrySkin: ×1.25 multiplier on received Fire damage. When the defender has
        // DrySkin, the same Fire damage can be explained by a lower defensive BSV.
        Ability::DrySkin,
    ]
    .iter()
    .filter(|a| !unknown_is_excluded(&mon.possible_abilities, a))
    .cloned()
    .collect();
    let neutral = neutral_ability(mon);
    if !abilities.contains(&neutral) {
        abilities.push(neutral);
    }
    abilities
}

/// Enumerate the distinct max-HP values the defender could have, given the
/// known species base stat, the defender's current IV/EV constraints, and the
/// current `[min_stats[0], max_stats[0]]` window.
///
/// **Soundness rationale (S-B fix):** Direction A samples the defender's possible
/// max-HP values to back-solve the defensive BSV from a percent-HP observation.
/// The true max-HP is exactly one value in `[hp_lo, hp_hi]`.  Iterating only a
/// stride-4 subset can skip achievable values whose feasible BSV interval extends
/// past the sampled union, causing `min_pre_nature_stat` to be raised above the
/// true value (unsound exclusion).  Iterating the exact EV-lattice HP values
/// eliminates that gap while remaining fast (at most 33 × max_ivs iterations).
///
/// When `config.force_max_ivs` is true the IV is fixed at 31; otherwise all 32
/// IVs are tried.  The returned list is sorted and deduplicated.
fn achievable_defender_hp_values(
    base_hp: u16,
    level: u8,
    config: &InferenceConfig,
    mon: &UnknownPokemonState,
) -> Vec<u16> {
    let hp_lo = mon.min_stats[0];
    let hp_hi = mon.max_stats[0];
    let iv_lo: u8 = if config.force_max_ivs { 31 } else { mon.min_ivs[0] };
    let iv_hi: u8 = if config.force_max_ivs { 31 } else { mon.max_ivs[0] };

    let mut vals: Vec<u16> = Vec::with_capacity(33);
    for iv in iv_lo..=iv_hi {
        for &ev in &EV_LATTICE {
            // Also respect the EV bounds tracked on the mon.
            if ev < mon.min_evs[0] || ev > mon.max_evs[0] {
                continue;
            }
            let hp = calc_hp(base_hp, iv, ev, level);
            if hp >= hp_lo && hp <= hp_hi && !vals.contains(&hp) {
                vals.push(hp);
            }
        }
    }
    if vals.is_empty() {
        // Fallback: should not happen if min_stats/max_stats are consistent, but
        // be sound by including both endpoints.
        vals.push(hp_lo);
        if hp_hi != hp_lo {
            vals.push(hp_hi);
        }
    }
    vals.sort_unstable();
    vals
}

/// The exact raw-HP interval that displays as percent `p` for a mon with `max_hp`.
///
/// Inverts `hp_to_percent` (round-half-up, 0 only at faint, 100 only at full,
/// otherwise clamped to 1–99) by direct enumeration — O(max_hp), trivially correct,
/// and negligible next to the Pass 3 oracle calls. Returns `None` when no raw HP
/// displays as `p` under this `max_hp` hypothesis (the hypothesis is then
/// incompatible with the observation and may be skipped — sound, it excludes only a
/// provably impossible world).
pub(crate) fn percent_bucket(p: u8, max_hp: u16) -> Option<(u16, u16)> {
    use crate::simulator::helpers::hp_to_percent;
    if p == 0 {
        return Some((0, 0));
    }
    if p >= 100 {
        return Some((max_hp, max_hp));
    }
    let mut lo: Option<u16> = None;
    let mut hi = 0u16;
    for hp in 1..max_hp {
        if hp_to_percent(hp, max_hp) == p {
            if lo.is_none() {
                lo = Some(hp);
            }
            hi = hp;
        }
    }
    lo.map(|l| (l, hi))
}

/// S22: the sound raw-damage interval for an observed `pre_pct → post_pct` display
/// transition under the max-HP hypothesis `max_hp`.
///
/// The former derivation treated `delta_pct = pre_pct − post_pct` as a single
/// rounding of the damage (`[(δ−0.5)%, (δ+0.5)%]` of max HP). But both endpoints
/// carry their own ±0.5% display rounding, so the true band is up to twice as wide —
/// for a 362-HP Blissey the old band could exclude several achievable damage values,
/// and with them the true defensive BSV (unsound exclusion). Only a `pre_pct` of 100
/// (full HP is displayed exactly) or a `post_pct` of 0 (faint is displayed exactly)
/// shrinks that side's rounding to zero — which the exact bucket intersection below
/// captures automatically.
fn percent_delta_damage_band(pre_pct: u8, post_pct: u8, max_hp: u16) -> Option<(u16, u16)> {
    let (pre_lo, pre_hi) = percent_bucket(pre_pct, max_hp)?;
    let (post_lo, post_hi) = percent_bucket(post_pct, max_hp)?;
    let d_lo = pre_lo.saturating_sub(post_hi).max(1);
    let d_hi = pre_hi.saturating_sub(post_lo);
    if d_hi == 0 { None } else { Some((d_lo, d_hi)) }
}

/// Per-nature-class neutral-gear BSV bounds produced by the Pass 3 oracle search.
/// Shared by both `pass3_direction_b` (attacker's offensive stat) and
/// `pass3_direction_a` (defender's defensive stat).
#[derive(Clone)]
struct NatureClassBound {
    mod_f32: f32,
    is_boost: bool,
    is_nerf: bool,
    bsv_lo_neutral: Option<u16>,
    bsv_hi_neutral: Option<u16>,
}

/// Shared return type of `compute_attacker_stat_bounds` (Direction B) and
/// `compute_defender_stat_bounds` (Direction A) — see either function's doc comment.
/// `(global_bsv_lo, global_bsv_hi, global_stat_lo, global_stat_hi, per_class,
/// primary-only CNF booster/reducer items, primary-only CNF booster/reducer
/// abilities, stat index)`.
type StatBoundsSearchResult =
    Option<(Option<u16>, Option<u16>, Option<u16>, Option<u16>, Vec<NatureClassBound>, Vec<Item>, Vec<Ability>, usize)>;

/// Returns the still-possible nature classes for `stat` on `possible_natures`.
///
/// Each triple is `(nat_mod, is_boost, is_nerf)`:
/// - boost   (×1.1, `is_boost = true`)
/// - neutral (×1.0)
/// - nerf    (×0.9, `is_nerf = true`)
///
/// The nerf class is always excluded when `si == 0` (HP), since no nature nerfs HP.
/// For Pass 3 callers this guard is always a no-op — neither `off_stat` nor
/// `def_stat` is ever HP — but it keeps the helper correct in isolation.
fn possible_nature_classes(
    possible_natures: &Unknown<Nature>,
    stat: &crate::state::dex_data::PokemonStat,
    si: usize,
) -> Vec<(f32, bool, bool)> {
    let boost_natures = bcp::boosting_natures_for_stat(stat);
    let nerf_natures  = bcp::nerfing_natures_for_stat(stat);
    let mut classes = Vec::new();
    if boost_natures.iter().any(|n| !unknown_is_excluded(possible_natures, n)) {
        classes.push((1.1_f32, true, false));
    }
    let any_neutral = ALL_NATURES.iter().any(|n| {
        !boost_natures.contains(n)
            && !nerf_natures.contains(n)
            && !unknown_is_excluded(possible_natures, n)
    });
    if any_neutral {
        classes.push((1.0_f32, false, false));
    }
    if si != 0 && nerf_natures.iter().any(|n| !unknown_is_excluded(possible_natures, n)) {
        classes.push((0.9_f32, false, true));
    }
    classes
}

/// Returns the spread-move damage multiplier for the current battle format.
///
/// ×0.75 in doubles when `move_data.target` hits all adjacent foes (or all adjacent),
/// ×1.0 otherwise.  Both Pass 3 directions apply this so the oracle and the observed
/// damage use the same scale.
fn spread_targets_mult(
    state: &UnknownBattleState,
    move_data: &crate::state::dex_data::MoveData,
) -> f64 {
    if state.active_per_side > 1
        && matches!(
            move_data.target,
            crate::state::dex_data::MoveTarget::AllAdjacent
                | crate::state::dex_data::MoveTarget::AllAdjacentFoes
        )
    {
        0.75
    } else {
        1.0
    }
}

/// Applies the globally tightest pre-nature and post-nature stat bounds for `mon_idx`
/// that were accumulated by the Pass 3 oracle union.
///
/// Only updates when the new bound is strictly tighter than the current tracked range,
/// preserving soundness (never expands a tracked range).
///
/// Thin `mon_idx` wrapper over [`apply_unconditional_tightening_to_mon`] — kept so the
/// primary path's call sites are unchanged. See that function for the actual logic and
/// for the hypothesis-mirroring use (Increment 2).
fn apply_unconditional_tightening(
    state: &mut UnknownBattleState,
    mon_idx: usize,
    si: usize,
    global_bsv_lo: Option<u16>,
    global_bsv_hi: Option<u16>,
    global_stat_lo: Option<u16>,
    global_stat_hi: Option<u16>,
) {
    if let Some(mon) = get_mon_mut_by_idx(state, mon_idx) {
        apply_unconditional_tightening_to_mon(
            mon, si, global_bsv_lo, global_bsv_hi, global_stat_lo, global_stat_hi,
        );
    }
}

/// Core of [`apply_unconditional_tightening`], operating directly on a mon rather than
/// a `mon_idx` lookup — this is what lets it also run against a live Zoroark hypothesis
/// (`possible_illusion_state`), which has no `mon_idx` of its own (Increment 2). Never
/// emits CNF (that stays primary-only via `emit_nature_conditional_bounds`, which a
/// hypothesis cannot be soundly addressed by either) — purely a direct field tightening,
/// so it's safe to call on any `UnknownPokemonState`, hypothetical or real.
fn apply_unconditional_tightening_to_mon(
    mon: &mut UnknownPokemonState,
    si: usize,
    global_bsv_lo: Option<u16>,
    global_bsv_hi: Option<u16>,
    global_stat_lo: Option<u16>,
    global_stat_hi: Option<u16>,
) {
    if let Some(lo) = global_bsv_lo
        && lo > mon.min_pre_nature_stat[si] {
            mon.min_pre_nature_stat[si] = lo;
        }
    if let Some(hi) = global_bsv_hi
        && hi < mon.max_pre_nature_stat[si] {
            mon.max_pre_nature_stat[si] = hi;
        }
    if let Some(lo) = global_stat_lo
        && lo > mon.min_stats[si] {
            mon.min_stats[si] = lo;
        }
    if let Some(hi) = global_stat_hi
        && hi < mon.max_stats[si] {
            mon.max_stats[si] = hi;
        }
}

/// Emits per-nature-class conditional CNF clauses bounding `mon_idx`'s pre-nature stat.
///
/// For each `cr` in `per_class`, emits a GE clause and a LE clause of the form:
///
/// ```text
/// [not-κ guards] ∨ EVIVStatGE/LE{bsv_lo/hi_neutral} ∨ ⋁ gear_items ∨ ⋁ gear_abilities
/// ```
///
/// `gear_items` / `gear_abilities` are the **alternative explanation** disjuncts:
/// - Direction B (offensive): booster items/abilities that could explain a higher raw stat.
/// - Direction A (defensive): reducer items/abilities that could explain a lower raw stat.
///
/// A clause is only emitted when:
/// - the neutral-gear BSV bound is known (`bsv_lo/hi_neutral = Some`), **and**
/// - the bound is strictly tighter than `current_pre_min` / `current_pre_max`.
///
/// When no gear alternatives are possible the clause degenerates to a singleton after
/// the guard, so the bound is forced directly into `min_pre/max_pre_nature_stat[si]`.
///
/// **Soundness invariant:** `current_pre_min/max` must be read from the pre-tightening
/// clone of the bounded mon (not the mutated state) so the "worth emitting" test is
/// stable throughout the loop.
fn emit_nature_conditional_bounds(
    state: &mut UnknownBattleState,
    mon_idx: usize,
    stat: &crate::state::dex_data::PokemonStat,
    per_class: &[NatureClassBound],
    gear_items: &[Item],
    gear_abilities: &[Ability],
    current_pre_min: u16,
    current_pre_max: u16,
) {
    for cr in per_class {
        let not_kappa_guards: Vec<Statement> = match (cr.is_boost, cr.is_nerf) {
            (true, _) => vec![Statement::Not(Box::new(Statement::NatureBoostsStat {
                mon_idx,
                stat: *stat,
            }))],
            (_, true) => vec![Statement::Not(Box::new(Statement::NatureNerfsStat {
                mon_idx,
                stat: *stat,
            }))],
            // neutral: exclude the class when the nature is definitely a booster or nerf
            (false, false) => vec![
                Statement::NatureBoostsStat { mon_idx, stat: *stat },
                Statement::NatureNerfsStat  { mon_idx, stat: *stat },
            ],
        };

        let gear_literals: Vec<Statement> = gear_items
            .iter()
            .map(|i| Statement::HasItem { mon_idx, item: i.clone() })
            .chain(gear_abilities.iter().map(|a| Statement::HasAbility {
                mon_idx,
                ability: a.clone(),
            }))
            .collect();

        // S35: always push the full clause (guards + bound literal + gear escapes) to
        // `state.predicates`, even when there are no gear escapes to include. The
        // previous code force-applied the bound directly to `min_pre_nature_stat`/
        // `max_pre_nature_stat` whenever `gear_literals` was empty — but `not_kappa_guards`
        // is a REAL condition (the nature must actually BE this boost/neutral/nerf class)
        // that has nothing to do with whether gear escapes exist; dropping it meant a
        // per-class-conditional bound ("IF nature nerfs this stat, BSV >= lo") got applied
        // unconditionally. Since `per_class` iterates every remaining nature class, this
        // let a nerf class's high `lo` and a boost class's low `hi` BOTH get force-applied
        // for the same mon/stat — producing an inverted `[min_pre_nature_stat > max_pre_nature_stat]`
        // window and the "every candidate nature is infeasible" pass5 contradiction on
        // ordinary turns. Pushing the clause instead lets BCP's existing fixpoint correctly
        // force the bound only once `not_kappa_guards` is independently proven false (i.e.
        // this class is confirmed), the same safe path the gear-escape branch already used.
        if let Some(lo) = cr.bsv_lo_neutral
            && lo > current_pre_min {
                let mut clause = not_kappa_guards.clone();
                clause.push(Statement::EVIVStatGE {
                    mon_idx,
                    stat: *stat,
                    value: lo,
                });
                clause.extend(gear_literals.clone());
                state.predicates.push(clause);
            }
        if let Some(hi) = cr.bsv_hi_neutral
            && hi < current_pre_max {
                let mut clause = not_kappa_guards.clone();
                clause.push(Statement::EVIVStatLE {
                    mon_idx,
                    stat: *stat,
                    value: hi,
                });
                clause.extend(gear_literals.clone());
                state.predicates.push(clause);
            }
    }
}

/// Direction B: we are the target, HP is exact, bound the ATTACKER's offensive BSV.
#[allow(clippy::too_many_arguments)]
fn pass3_direction_b(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    ctx: &BattleContext,
    user_idx: usize,
    target_idx: usize,
    user_slot: &FieldSlot,
    target_slot: &FieldSlot,
    move_data: &crate::state::dex_data::MoveData,
    off_stat: &crate::state::dex_data::PokemonStat,
    is_crit: bool,
    // S23: the HP the target was at when THIS hit landed (pass 1 has already applied
    // the whole reaction tree, so the live field holds the post-move HP — wrong for
    // full-HP-gated reducers like Multiscale on the hit that broke full HP).
    hit_pre_hp: &PokemonHP,
    exact_damage: u16,
    // Per-hit base power override for multi-hit moves (None = use move's base_power).
    bp_override: Option<u16>,
    // True for Gyro Ball / Electro Ball — BP depends on attacker + target speeds.
    speed_dep_bp: bool,
) {
    // S24: enumerate items/abilities and materialize from the PRE-MOVE snapshots —
    // the live mons already carry this move's own reactions (self-boosts, mid-move
    // status, consumed items), which must not leak into the oracle run for a hit
    // that happened before them. Bounds are still written back to the live mon by
    // apply_unconditional_tightening. Falls back to the live mon when no snapshot
    // exists (defensive; MoveUsed always populates it).
    //
    // This snapshot ALSO carries a clone of the attacker's `possible_illusion_state`
    // as it stood pre-move (Rust's derive(Clone) on `Option<Box<UnknownPokemonState>>`
    // deep-clones the box) — Increment 2's hypothesis mirror below reads it directly
    // from here rather than re-fetching, for the exact same S24 pre-move reason.
    let attacker_unk = ctx
        .move_context
        .as_ref()
        .and_then(|mc| mc.pre_move_attacker.clone())
        .or_else(|| get_mon_by_idx(state, user_idx).cloned());
    let Some(attacker_unk) = attacker_unk else {
        return;
    };
    // S26: a Transformed attacker's Atk/SpA are COPIED from the copy source, not
    // derived from its own species base — the BSV inversion here would bound the
    // wrong thing. Skip (the copy source's own stat inference stands on its own).
    if attacker_unk.pre_transform.is_some() {
        return;
    }
    let target_unk = ctx
        .move_context
        .as_ref()
        .and_then(|mc| {
            mc.pre_move_targets
                .iter()
                .find(|(slot, _)| slot == target_slot)
                .map(|(_, m)| m.clone())
        })
        .or_else(|| get_mon_by_idx(state, target_idx).cloned());
    let Some(mut target_unk) = target_unk else {
        return;
    };
    // S23: materialize the target at the HP this hit was actually taken at.
    target_unk.hp = hit_pre_hp.clone();

    if let Some((global_bsv_lo, global_bsv_hi, global_stat_lo, global_stat_hi, per_class, booster_items, booster_abilities, si)) =
        compute_attacker_stat_bounds(
            state, &attacker_unk, &target_unk, user_slot, target_slot, move_data, off_stat,
            ctx, is_crit, exact_damage, bp_override, speed_dep_bp,
        )
    {
        // Apply unconditional tightening.
        apply_unconditional_tightening(
            state, user_idx, si,
            global_bsv_lo, global_bsv_hi, global_stat_lo, global_stat_hi,
        );

        // ── Conditional CNF predicates ────────────────────────────────────────
        // For each nature class κ, emit nature-guarded GE/LE clauses with booster
        // disjuncts. `current_pre_min/max` are read from the pre-tightening clone
        // (attacker_unk) so the "worth emitting" gate is stable throughout the loop.
        emit_nature_conditional_bounds(
            state, user_idx, off_stat,
            &per_class, &booster_items, &booster_abilities,
            attacker_unk.min_pre_nature_stat[si],
            attacker_unk.max_pre_nature_stat[si],
        );
    }

    // Increment 2: mirror the same search onto a live Zoroark hypothesis, if the
    // attacker's pre-move snapshot carried one. Never emits CNF (no sound `mon_idx`
    // to key a hypothesis's clause to) — see the plan's scope note.
    if let Some(hyp) = attacker_unk.possible_illusion_state.clone() {
        mirror_pass3_direction_b_onto_hypothesis(
            state, user_idx, target_idx, *hyp, &target_unk, user_slot, target_slot,
            move_data, off_stat, ctx, is_crit, exact_damage, bp_override, speed_dep_bp,
        );
    }
}

/// Core Direction B search: for a candidate attacker (`attacker_unk`) that dealt
/// `exact_damage` to `target_unk` with `move_data`, enumerates nature classes × items
/// × abilities × Metronome streak × pinch-HP hypotheses, binary-searching the feasible
/// BSV interval via `find_feasible_bsv_range_b` for each combination and unioning into
/// a single tightest-known window. Extracted verbatim from `pass3_direction_b` so the
/// SAME search can run against either the real attacker (primary) or a live Zoroark
/// hypothesis (Increment 2) with zero risk of the two hypotheses' math drifting apart.
///
/// Pure computation over its value parameters: `state`/`ctx` are read-only oracle
/// context (field/weather materialization, `Analytic` last-mover lookup), never
/// mutated — `attacker_unk`/`target_unk` are the ONLY things the returned bounds
/// depend on.
///
/// Returns `None` for any of the early-return conditions the primary path always
/// had (unknown attacker species, empty pre-nature window, no possible nature class)
/// — callers must treat that as "no new evidence from this hit," never a
/// contradiction (absence of derivable evidence is never grounds to drop a
/// hypothesis). Returns `Some((global_bsv_lo, global_bsv_hi, global_stat_lo,
/// global_stat_hi, per_class, booster_items, booster_abilities, si))` otherwise —
/// `per_class`/`booster_items`/`booster_abilities` are only consumed by the primary's
/// CNF emission, but are returned uniformly so the primary call site is unchanged.
#[allow(clippy::too_many_arguments)]
fn compute_attacker_stat_bounds(
    state: &UnknownBattleState,
    attacker_unk: &UnknownPokemonState,
    target_unk: &UnknownPokemonState,
    user_slot: &FieldSlot,
    target_slot: &FieldSlot,
    move_data: &crate::state::dex_data::MoveData,
    off_stat: &crate::state::dex_data::PokemonStat,
    ctx: &BattleContext,
    is_crit: bool,
    exact_damage: u16,
    bp_override: Option<u16>,
    speed_dep_bp: bool,
) -> StatBoundsSearchResult {
    use crate::simulator::DamageConfig;

    // Need known attacker species for BSV-based inference.
    let base_stats = match &attacker_unk.possible_species {
        Unknown::Known(s) => match ctx.dex.get(s) {
            Some(d) => d.base_stats,
            None => return None,
        },
        _ => return None,
    };

    let si = bcp::stat_to_stats_idx(off_stat);

    // Current pre-nature BSV range for this stat.
    let bsv_lo = attacker_unk.min_pre_nature_stat[si];
    let bsv_hi = attacker_unk.max_pre_nature_stat[si];
    if bsv_lo > bsv_hi {
        return None;
    }

    // Determine which nature classes are still possible.
    let nature_classes = possible_nature_classes(&attacker_unk.possible_natures, off_stat, si);
    if nature_classes.is_empty() {
        return None;
    }

    // Booster sets for predicate emission.
    // E-B optimisation: prune entries provably inert for this move.
    // The attacker is the OPPONENT (unknown ability), so we use `move_data.pokemon_type`
    // as a conservative effective type — this is sound because all type-converting
    // abilities (LiquidVoice, -ate) are themselves already in `booster_abilities` and
    // therefore in the oracle run; non-type-converting entries use the raw type.
    let eff_type_b = move_data.pokemon_type.clone();
    let booster_items = {
        let mut items = offensive_damage_items(attacker_unk);
        items.retain(|item| {
            match item {
                Item::MuscleBand  => matches!(move_data.category, MoveCategory::Physical),
                Item::WiseGlasses => matches!(move_data.category, MoveCategory::Special),
                _ => {
                    if let Some(boost_type) = type_boosting_item_type(item) {
                        return boost_type == eff_type_b;
                    }
                    true
                }
            }
        });
        items
    };
    let booster_abilities = {
        let mut abilities = offensive_damage_abilities(attacker_unk);
        abilities.retain(|ability| {
            match ability {
                Ability::IronFist     => move_has_flag(move_data, &MoveFlag::Punch),
                Ability::StrongJaw    => move_has_flag(move_data, &MoveFlag::Bite),
                Ability::Sharpness    => move_has_flag(move_data, &MoveFlag::Slicing),
                Ability::MegaLauncher => move_has_flag(move_data, &MoveFlag::Pulse),
                Ability::ToughClaws   => move_has_flag(move_data, &MoveFlag::Contact),
                Ability::WaterBubble  => matches!(eff_type_b, PokemonType::Water),
                Ability::FairyAura    => matches!(eff_type_b, PokemonType::Fairy),
                Ability::LiquidVoice  => move_has_flag(move_data, &MoveFlag::Sound),
                // MegaSol: perceives Sun → affects Fire (×1.5) and Water (×0.5) only.
                Ability::MegaSol => matches!(eff_type_b, PokemonType::Fire | PokemonType::Water),
                // Analytic: ×1.3 only when the holder moves LAST this turn.
                // The oracle's empty action_queue makes attacker_is_last_mover always
                // true, so keep the ability in the union only when the attacker really
                // moved last (S28: precomputed from the event stream — exact in singles
                // and doubles, and correct when the last actor flinched or switched).
                // Keeping it when Analytic did NOT fire would inflate the oracle output
                // and push the feasible-BSV upper bound below truth (unsound exclusion).
                Ability::Analytic => analytic_fired(ctx, user_slot),
                // Type-converting abilities (-ate, Normalize): keep for Normal moves to allow
                // conversion; for non-Normal we conservatively keep them too because an -ate
                // attacker with an -ate ability may have a different oracle type (soundness).
                _ => true,
            }
        });
        abilities
    };

    // Oracle config: always use all 16 rolls regardless of CLI setting.
    let oracle_config = DamageConfig {
        consider_crit: true,
        damage_rolls: 16,
        sample: false,
    };

    // Spread multiplier (doubles ×0.75 when a move hits all adjacent foes with 2+ active opponents).
    let targets_mult = spread_targets_mult(state, move_data);

    // For speed-dependent BP moves (Gyro Ball / Electro Ball), the attacker's speed
    // also varies over its current [min, max] range. We scan both endpoints (sound
    // because BP is monotone in the speed ratio — all intermediate BPs are covered).
    let attacker_speed_range: Option<(u16, u16)> = if speed_dep_bp {
        Some((attacker_unk.min_stats[5], attacker_unk.max_stats[5]))
    } else {
        None
    };

    // ── S25: attacker HP hypotheses for the pinch-ability gate ────────────────
    // Blaze/Overgrow/Swarm/Torrent gate on `hp*3 <= max` in the oracle, but the
    // materialize sentinel maps any non-100 display percent to 0.5×max — never
    // active. Decide which gate states the attacker's display percent admits and
    // enumerate each as its own oracle hypothesis (`None` = keep the snapshot HP
    // as-is, i.e. the non-pinch sentinel path; `Some(hp)` = override with a Number
    // strictly inside the gate — Number passes through materialize untouched, and
    // `make_atk` leaves stats[0] at min_stats[0], which is what the override is
    // computed against). Thresholds are widened by 1% on each side of the exact
    // 33.33% bucket edge so display-rounding jitter can never drop a possible
    // hypothesis (extra variants only widen the union — sound).
    let attacker_hp_variants: Vec<Option<PokemonHP>> = match &attacker_unk.hp {
        // Exact HP (or full HP): the gate is already evaluated correctly.
        PokemonHP::Number(_) | PokemonHP::Percent(100) => vec![None],
        PokemonHP::Percent(p) => {
            let pinch_hp = PokemonHP::Number((attacker_unk.min_stats[0] / 4).max(1));
            if *p <= 31 {
                vec![Some(pinch_hp)] // certainly at ≤1/3: pinch abilities are live
            } else if *p <= 34 {
                vec![Some(pinch_hp), None] // bucket straddles the gate: union both
            } else {
                vec![None] // certainly above 1/3: sentinel path is correct
            }
        }
    };

    // ── Unconditional tightening: union over all (nature_class, item, ability) ─
    let mut global_bsv_lo: Option<u16> = None;
    let mut global_bsv_hi: Option<u16> = None;
    let mut global_stat_lo: Option<u16> = None;
    let mut global_stat_hi: Option<u16> = None;

    // ── Per-nature-class data for predicate emission ──────────────────────────
    // For each nature class we compute the BSV interval under neutral gear (no booster).
    let mut per_class: Vec<NatureClassBound> = Vec::new();

    for (nat_mod, is_boost, is_nerf) in &nature_classes {
        // Items to enumerate for this class: all possible items (for union) plus
        // a neutral-item run (for predicate lower bound).
        let item_choices: Vec<Item> = {
            let mut items = booster_items.clone();
            let neutral = neutral_item(attacker_unk);
            if !items.contains(&neutral) {
                items.push(neutral);
            }
            items
        };
        let ability_choices: Vec<Ability> = {
            let mut abs = booster_abilities.clone();
            let neutral = neutral_ability(attacker_unk);
            if !abs.contains(&neutral) {
                abs.push(neutral);
            }
            abs
        };

        // Also enumerate Metronome item streak values 0..=5.
        let streak_range: Vec<u8> = if !unknown_is_excluded(&attacker_unk.item, &Item::Metronome) {
            vec![0, 1, 2, 3, 4, 5]
        } else {
            vec![0]
        };

        // ── Neutral-gear BSV interval (for predicate emission) ─────────────
        // S25: union across the attacker HP hypotheses — a Known pinch ability
        // (neutral_a) needs the correct gate state, and a wider neutral interval
        // only loosens the emitted GE/LE clause bounds (sound).
        let neutral_i = neutral_item(attacker_unk);
        let neutral_a = neutral_ability(attacker_unk);

        let mut bsv_lo_neutral: Option<u16> = None;
        let mut bsv_hi_neutral: Option<u16> = None;
        for hp_variant in &attacker_hp_variants {
            let mut atk_neutral = attacker_unk.clone();
            if let Some(hp) = hp_variant {
                atk_neutral.hp = hp.clone();
            }
            let (lo, hi) = find_feasible_bsv_range_b(
                state,
                &atk_neutral,
                target_unk,
                user_slot,
                target_slot,
                move_data,
                &oracle_config,
                targets_mult,
                *nat_mod,
                si,
                base_stats,
                bsv_lo,
                bsv_hi,
                neutral_i.clone(),
                neutral_a.clone(),
                0,
                exact_damage,
                is_crit,
                bp_override,
                attacker_speed_range,
            );
            if let Some(lo_v) = lo {
                bsv_lo_neutral = Some(bsv_lo_neutral.map_or(lo_v, |g: u16| g.min(lo_v)));
            }
            if let Some(hi_v) = hi {
                bsv_hi_neutral = Some(bsv_hi_neutral.map_or(hi_v, |g: u16| g.max(hi_v)));
            }
        }

        per_class.push(NatureClassBound {
            mod_f32: *nat_mod,
            is_boost: *is_boost,
            is_nerf: *is_nerf,
            bsv_lo_neutral,
            bsv_hi_neutral,
        });

        // ── Union over all (item, ability, streak, hp-hypothesis) assignments ──
        for item in &item_choices {
            for ability in &ability_choices {
                for &streak in &streak_range {
                    for hp_variant in &attacker_hp_variants {
                        let mut atk_for_oracle = attacker_unk.clone();
                        atk_for_oracle.consecutive_move_count = streak;
                        if let Some(hp) = hp_variant {
                            atk_for_oracle.hp = hp.clone();
                        }

                        let (lo, hi) = find_feasible_bsv_range_b(
                            state,
                            &atk_for_oracle,
                            target_unk,
                            user_slot,
                            target_slot,
                            move_data,
                            &oracle_config,
                            targets_mult,
                            *nat_mod,
                            si,
                            base_stats,
                            bsv_lo,
                            bsv_hi,
                            item.clone(),
                            ability.clone(),
                            streak,
                            exact_damage,
                            is_crit,
                            bp_override,
                            attacker_speed_range,
                        );
                        if let (Some(lo_v), Some(hi_v)) = (lo, hi) {
                            let final_lo = (lo_v as f64 * *nat_mod as f64).floor() as u16;
                            let final_hi = (hi_v as f64 * *nat_mod as f64).floor() as u16;
                            global_bsv_lo = Some(global_bsv_lo.map_or(lo_v, |g| g.min(lo_v)));
                            global_bsv_hi = Some(global_bsv_hi.map_or(hi_v, |g| g.max(hi_v)));
                            global_stat_lo =
                                Some(global_stat_lo.map_or(final_lo, |g| g.min(final_lo)));
                            global_stat_hi =
                                Some(global_stat_hi.map_or(final_hi, |g| g.max(final_hi)));
                        }
                    }
                }
            }
        }
    }

    Some((
        global_bsv_lo, global_bsv_hi, global_stat_lo, global_stat_hi,
        per_class, booster_items, booster_abilities, si,
    ))
}

/// Write a Pass-3-derived hypothesis value back onto `idx`'s LIVE mon — but only
/// if that mon is still ambiguous right now. `value` is computed from a pre-move
/// snapshot (`defender_unk`/`attacker_unk`) taken before Pass 1 ran; between that
/// snapshot and this write, an earlier reaction in the SAME event-tree walk (most
/// commonly a direct-damage `IllusionEnded`, or the mon's own learnset-illegal
/// move) may have already promoted or rejected this mon's hypothesis via
/// `promote_illusion_to_primary`/`resolve_zoroark_globally` — `possible_illusion_
/// state` is already `None` on the live mon in that case, and writing `value`
/// back here would silently un-resolve an already-settled identity, leaving
/// `is_illusion_suspected` stuck `true` even though species/ability/moves have
/// already flipped to the resolved truth. S42.
fn write_back_pass3_hypothesis(
    state: &mut UnknownBattleState,
    idx: usize,
    value: Option<Box<UnknownPokemonState>>,
) {
    if let Some(mon) = get_mon_mut_by_idx(state, idx)
        && mon.possible_illusion_state.is_some()
    {
        mon.possible_illusion_state = value;
    }
}

/// Mirrors Direction B's tightening onto a live Zoroark hypothesis (Increment 2).
/// `hyp` is the attacker's pre-move hypothesis snapshot (from `attacker_unk`'s own
/// cloned `possible_illusion_state` — see `pass3_direction_b`'s S24 comment). Unlike
/// the primary path, never emits CNF (no sound `mon_idx` to key a hypothesis's
/// clause to — see the plan's scope note) and detects infeasibility via an inline
/// inverted-window check, since Pass 3 itself never panics — nothing else will
/// discover a hypothesis-only contradiction if this doesn't.
#[allow(clippy::too_many_arguments)]
fn mirror_pass3_direction_b_onto_hypothesis(
    state: &mut UnknownBattleState,
    user_idx: usize,
    target_idx: usize,
    mut hyp: UnknownPokemonState,
    target_unk: &UnknownPokemonState,
    user_slot: &FieldSlot,
    target_slot: &FieldSlot,
    move_data: &crate::state::dex_data::MoveData,
    off_stat: &crate::state::dex_data::PokemonStat,
    ctx: &BattleContext,
    is_crit: bool,
    exact_damage: u16,
    bp_override: Option<u16>,
    speed_dep_bp: bool,
) {
    // Doubles/ally-hit edge case: if the target ALSO carries a live hypothesis (both
    // sides of this hit are ambiguous), skip tightening entirely — sound (declining
    // to extract information from an ambiguous-vs-ambiguous hit is always safe; an
    // unmodeled joint approximation is not). In practice Direction B only ever fires
    // when the target is the viewer's own (Number-HP-tracked) mon, which never
    // carries a hypothesis, so this is a defensive guard, not a live path today.
    let target_has_hyp = get_mon_by_idx(state, target_idx)
        .is_some_and(|t| t.possible_illusion_state.is_some());
    // S26 (mirrored): a Transformed hypothesis can't be soundly analyzed this way.
    if target_has_hyp || hyp.pre_transform.is_some() {
        write_back_pass3_hypothesis(state, user_idx, Some(Box::new(hyp)));
        return;
    }

    let feasible = match compute_attacker_stat_bounds(
        state, &hyp, target_unk, user_slot, target_slot, move_data, off_stat,
        ctx, is_crit, exact_damage, bp_override, speed_dep_bp,
    ) {
        None => true, // no new evidence derivable from this hit for this identity
        Some((bsv_lo, bsv_hi, stat_lo, stat_hi, _per_class, _items, _abilities, si)) => {
            apply_unconditional_tightening_to_mon(&mut hyp, si, bsv_lo, bsv_hi, stat_lo, stat_hi);
            hyp.min_pre_nature_stat[si] <= hyp.max_pre_nature_stat[si]
                && hyp.min_stats[si] <= hyp.max_stats[si]
        }
    };

    write_back_pass3_hypothesis(state, user_idx, feasible.then_some(Box::new(hyp)));
}

/// Generic monotone binary-search for the feasible BSV interval `[found_lo, found_hi]`.
///
/// The three injected predicates encode the direction of the monotonicity:
///
/// - `p_lo(bsv)` — `true` for BSVs that are *at least* as strong as a candidate
///   lower bracket.  The search finds the **smallest** bsv satisfying this.
///   - Offensive (B): `roll_band(b).1 >= exact_damage`  (max_roll ≥ target)
///   - Defensive (A): `roll_band(b).0 <= d_hi`          (min_roll ≤ damage cap)
///
/// - `p_hi(bsv)` — finds the **largest** bsv satisfying this.
///   - Offensive (B): `roll_band(b).0 <= exact_damage`
///   - Defensive (A): `roll_band(b).1 >= d_lo`
///
/// - `can_produce(bsv)` — exact feasibility: used for the short linear refine walk
///   inside `[bracket_lo, bracket_hi]`.
///
/// Returns `(None, None)` when no BSV in `[bsv_lo, bsv_hi]` is feasible.
fn bracketed_feasible_bsv_range(
    bsv_lo: u16,
    bsv_hi: u16,
    p_lo: impl Fn(u16) -> bool,
    p_hi: impl Fn(u16) -> bool,
    can_produce: impl Fn(u16) -> bool,
) -> (Option<u16>, Option<u16>) {
    // Lower bracket: smallest bsv satisfying p_lo.
    let bracket_lo: Option<u16> = {
        let (mut lo, mut hi) = (bsv_lo as i32, bsv_hi as i32);
        let mut found = None;
        while lo <= hi {
            let mid = (lo + hi) / 2;
            if p_lo(mid as u16) {
                found = Some(mid as u16);
                hi = mid - 1;
            } else {
                lo = mid + 1;
            }
        }
        found
    };
    // Upper bracket: largest bsv satisfying p_hi.
    let bracket_hi: Option<u16> = {
        let (mut lo, mut hi) = (bsv_lo as i32, bsv_hi as i32);
        let mut found = None;
        while lo <= hi {
            let mid = (lo + hi) / 2;
            if p_hi(mid as u16) {
                found = Some(mid as u16);
                lo = mid + 1;
            } else {
                hi = mid - 1;
            }
        }
        found
    };
    match (bracket_lo, bracket_hi) {
        (Some(bl), Some(bh)) if bl <= bh => {
            // Refine: linear walk from each bracket end to the first actually-feasible BSV.
            // In practice ≤ 1–2 steps since the band bracket is tight.
            let found_lo = (bl..=bh).find(|&b| can_produce(b));
            let found_hi = (bl..=bh).rev().find(|&b| can_produce(b));
            (found_lo, found_hi)
        }
        _ => (None, None),
    }
}

/// Linear scan for the feasible BSV interval [lo, hi] for Direction B under a
/// single fixed (nat_mod, item, ability, streak) assignment.
///
/// Returns `(Some(lo), Some(hi))` if any BSV in `[bsv_lo, bsv_hi]` can produce
/// `exact_damage`, `(None, None)` if none can.
///
/// `bp_override` — per-hit base power (for multi-hit moves); `None` uses move's BP.
/// `attacker_speed_range` — for Gyro Ball / Electro Ball, the attacker's speed stat
/// range to union over. The oracle is called at the speed endpoints (sound because
/// BP is monotone in the speed ratio — all intermediate BPs lie in between).
#[allow(clippy::too_many_arguments)]
fn find_feasible_bsv_range_b(
    state: &UnknownBattleState,
    attacker_unk: &UnknownPokemonState,
    target_unk: &UnknownPokemonState,
    user_slot: &FieldSlot,
    target_slot: &FieldSlot,
    move_data: &crate::state::dex_data::MoveData,
    oracle_config: &crate::simulator::DamageConfig,
    targets_mult: f64,
    nat_mod: f32,
    si: usize,
    base_stats: [u16; 6],
    bsv_lo: u16,
    bsv_hi: u16,
    item: Item,
    ability: Ability,
    streak: u8,
    exact_damage: u16,
    is_crit: bool,
    bp_override: Option<u16>,
    attacker_speed_range: Option<(u16, u16)>,
) -> (Option<u16>, Option<u16>) {
    use crate::information::materialize::{materialize_battle, materialize_pokemon};
    use crate::simulator::helpers::calculate_damage_outcomes_for_target_with_options;

    // Materialize target with known stats (target is our own mon — stats exact).
    let target_stats = target_unk.min_stats; // min == max for known mons
    let target_ps = materialize_pokemon(
        target_unk,
        target_stats,
        neutral_item(target_unk),
        neutral_ability(target_unk),
    );

    // Speed endpoints for speed-dependent BP moves (Gyro Ball / Electro Ball).
    // Scanning both endpoints of [min_spe, max_spe] is sound since BP is monotone
    // in the speed ratio, covering all intermediate BPs.
    let speed_endpoints: Vec<u16> = match attacker_speed_range {
        Some((lo, hi)) if lo != hi => vec![lo, hi],
        Some((lo, _)) => vec![lo],
        None => vec![attacker_unk.min_stats[5]],
    };

    // Build the attacker PS with a specific BSV and optional speed override.
    // Clone once here (per find_feasible_bsv_range_b call) instead of once per BSV probe
    // inside make_atk. The caller already sets consecutive_move_count = streak on the
    // passed-in attacker_unk, so no additional mutation is needed.
    let atk_unk_base = attacker_unk.clone();
    let make_atk = |bsv: u16, spe_override: u16| -> crate::state::pokemon::PokemonState {
        let mut stats = atk_unk_base.min_stats; // fill non-inferred stats with current min
        if si == 0 {
            stats[0] = bsv; // HP: no nature
        } else {
            stats[si] = (bsv as f64 * nat_mod as f64).floor() as u16;
        }
        stats[5] = spe_override; // override speed (no-op for non-speed-dep moves)
        materialize_pokemon(&atk_unk_base, stats, item.clone(), ability.clone())
    };

    // Build oracle outcomes for a single (bsv, spe_override) pair.
    //
    // The battle skeleton is materialized ONCE and the attacker swapped in per probe:
    // materialize_battle clones every field vector, and doing that inside the binary
    // search (probes × speed endpoints × item/ability configs) dominated Pass-3 setup
    // cost. RefCell because `run_oracle` is shared by the can_produce / roll_band
    // closures below.
    let atk_is_p1 = user_slot.player == crate::state::battle::Player::P1;
    let initial_atk = make_atk(bsv_lo, speed_endpoints[0]);
    let battle_cell = std::cell::RefCell::new(if atk_is_p1 {
        materialize_battle(state, vec![initial_atk], vec![target_ps.clone()])
    } else {
        materialize_battle(state, vec![target_ps.clone()], vec![initial_atk])
    });
    let run_oracle = |bsv: u16, spe: u16| -> Vec<(u16, bool, f64)> {
        let atk_ps = make_atk(bsv, spe);
        let mut battle = battle_cell.borrow_mut();
        if atk_is_p1 {
            battle.p1_active_mons[0] = atk_ps.clone();
        } else {
            battle.p2_active_mons[0] = atk_ps.clone();
        }
        calculate_damage_outcomes_for_target_with_options(
            &battle,
            &atk_ps,
            &target_ps,
            *user_slot,
            *target_slot,
            move_data,
            *oracle_config,
            targets_mult,
            1.0,
            bp_override,
            None,
        )
    };

    // A BSV is feasible if the oracle produces `exact_damage` with correct crit for
    // *any* speed endpoint (sound union over the speed range).
    let can_produce = |bsv: u16| -> bool {
        speed_endpoints.iter().any(|&spe| {
            run_oracle(bsv, spe)
                .iter()
                .any(|(dmg, crit, _)| *dmg == exact_damage && *crit == is_crit)
        })
    };

    // (min, max) damage for outcomes matching the crit flag, unioned over speed endpoints.
    // Monotone: attacker offense ↑ as bsv ↑, so min_dmg and max_dmg are non-decreasing.
    let roll_band = |bsv: u16| -> (Option<u16>, Option<u16>) {
        let mut lo: Option<u16> = None;
        let mut hi: Option<u16> = None;
        for &spe in &speed_endpoints {
            for (dmg, crit, _) in run_oracle(bsv, spe) {
                if crit == is_crit {
                    lo = Some(lo.map_or(dmg, |m: u16| m.min(dmg)));
                    hi = Some(hi.map_or(dmg, |m: u16| m.max(dmg)));
                }
            }
        }
        (lo, hi)
    };

    // Binary-search for the feasible BSV interval, exploiting the monotone damage
    // property (higher offensive BSV → more damage, non-decreasing).
    //
    // Feasibility: min_roll(bsv) ≤ exact_damage ≤ max_roll(bsv).
    //   p_lo: smallest bsv where max_roll ≥ exact_damage
    //   p_hi: largest  bsv where min_roll ≤ exact_damage
    bracketed_feasible_bsv_range(
        bsv_lo,
        bsv_hi,
        |b| roll_band(b).1.is_some_and(|m| m >= exact_damage),
        |b| roll_band(b).0.is_some_and(|m| m <= exact_damage),
        can_produce,
    )
}

/// Feasible BSV interval for Direction A under a single fixed
/// `(nat_mod, hp_cand, def_item, def_ability)` assignment.
///
/// Mirrors [`find_feasible_bsv_range_b`] but for the defensive direction:
/// - The **attacker** is our known mon; `atk_ps` is materialized once by the caller.
/// - The **defender's** stat is varied; damage is non-increasing in the defensive BSV.
/// - The damage observation is a percent-derived interval `[d_lo, d_hi]` rather than
///   an exact value.
/// - Defensive items (AssaultVest, Eviolite) are stat-baked directly into the oracle
///   (the standard oracle only handles offensive item multipliers).
///
/// Returns `(Some(lo), Some(hi))` if any BSV in `[bsv_lo, bsv_hi]` can produce a
/// damage value in `[d_lo, d_hi]`, `(None, None)` otherwise.
#[allow(clippy::too_many_arguments)]
fn find_feasible_bsv_range_a(
    state: &UnknownBattleState,
    defender_unk: &UnknownPokemonState,
    atk_ps: &crate::state::pokemon::PokemonState,
    user_slot: &FieldSlot,
    target_slot: &FieldSlot,
    move_data: &crate::state::dex_data::MoveData,
    oracle_config: crate::simulator::DamageConfig,
    targets_mult: f64,
    nat_mod: f32,
    si: usize,
    bsv_lo: u16,
    bsv_hi: u16,
    hp_cand: u16,
    def_item: &Item,
    def_ability: &Ability,
    def_speed_endpoints: &[u16],
    d_lo: u16,
    d_hi: u16,
    is_crit: bool,
    bp_override: Option<u16>,
) -> (Option<u16>, Option<u16>) {
    use crate::information::materialize::{materialize_battle, materialize_pokemon};
    use crate::simulator::helpers::calculate_damage_outcomes_for_target_with_options;

    // Build and run the damage oracle for a fixed (bsv, def_spe) pair.
    // Pre-bakes defensive item stat multiplier (AV ×1.5 SpD, Eviolite ×1.5 Def+SpD)
    // since the standard oracle only handles offensive item multipliers.
    //
    // The battle skeleton is materialized ONCE with a placeholder in the defender
    // slot and swapped per probe — see find_feasible_bsv_range_b for rationale.
    let atk_is_p1 = user_slot.player == crate::state::battle::Player::P1;
    let battle_cell = std::cell::RefCell::new(materialize_battle(
        state,
        vec![atk_ps.clone()],
        vec![atk_ps.clone()],
    ));
    let run_oracle = |bsv: u16, def_spe: u16| -> Vec<(u16, bool, f64)> {
        let item_stat_mult: f64 = match def_item {
            Item::AssaultVest
                if matches!(move_data.category, MoveCategory::Special) => 1.5,
            Item::Eviolite => 1.5,
            _ => 1.0,
        };
        let mut def_stats = defender_unk.min_stats;
        def_stats[0] = hp_cand;
        if si == 0 {
            def_stats[0] = bsv;
        } else {
            let raw = (bsv as f64 * nat_mod as f64).floor() as u16;
            def_stats[si] = if item_stat_mult != 1.0 {
                (raw as f64 * item_stat_mult).floor() as u16
            } else {
                raw
            };
        }
        def_stats[5] = def_spe;
        let def_ps = materialize_pokemon(
            defender_unk, def_stats, def_item.clone(), def_ability.clone(),
        );
        let mut battle = battle_cell.borrow_mut();
        if atk_is_p1 {
            battle.p1_active_mons[0] = atk_ps.clone();
            battle.p2_active_mons[0] = def_ps.clone();
        } else {
            battle.p1_active_mons[0] = def_ps.clone();
            battle.p2_active_mons[0] = atk_ps.clone();
        }
        calculate_damage_outcomes_for_target_with_options(
            &battle, atk_ps, &def_ps,
            *user_slot, *target_slot,
            move_data, oracle_config, targets_mult, 1.0, bp_override, None,
        )
    };

    // (min_dmg, max_dmg) for crit-matched outcomes, unioned over speed endpoints.
    let roll_band = |bsv: u16| -> (Option<u16>, Option<u16>) {
        let mut lo: Option<u16> = None;
        let mut hi: Option<u16> = None;
        for &def_spe in def_speed_endpoints {
            for (dmg, crit, _) in run_oracle(bsv, def_spe) {
                if crit == is_crit {
                    lo = Some(lo.map_or(dmg, |m: u16| m.min(dmg)));
                    hi = Some(hi.map_or(dmg, |m: u16| m.max(dmg)));
                }
            }
        }
        (lo, hi)
    };

    let can_produce = |bsv: u16| -> bool {
        def_speed_endpoints.iter().any(|&def_spe| {
            run_oracle(bsv, def_spe)
                .iter()
                .any(|(dmg, crit, _)| *dmg >= d_lo && *dmg <= d_hi && *crit == is_crit)
        })
    };

    // Defensive monotonicity: higher defensive BSV → less damage (non-increasing).
    //   p_lo: smallest bsv where min_roll ≤ d_hi
    //   p_hi: largest  bsv where max_roll ≥ d_lo
    bracketed_feasible_bsv_range(
        bsv_lo,
        bsv_hi,
        |b| roll_band(b).0.is_some_and(|m| m <= d_hi),
        |b| roll_band(b).1.is_some_and(|m| m >= d_lo),
        can_produce,
    )
}

/// Direction A: we attacked the opponent, HP is a percent interval,
/// bound the DEFENDER's defensive stat (and HP).
#[allow(clippy::too_many_arguments)]
fn pass3_direction_a(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    ctx: &BattleContext,
    user_idx: usize,
    target_idx: usize,
    user_slot: &FieldSlot,
    target_slot: &FieldSlot,
    move_data: &crate::state::dex_data::MoveData,
    def_stat: &crate::state::dex_data::PokemonStat,
    is_crit: bool,
    // S23: the HP the defender was at when THIS hit landed (the live field holds the
    // post-move HP by the time Pass 3 runs — see pass3_direction_b).
    hit_pre_hp: &PokemonHP,
    // S22: pre-hit and post-hit DISPLAY percents. Each carries its own display
    // rounding; the damage band is derived per max-HP hypothesis from the exact
    // display buckets, not from the delta alone.
    pre_pct: u8,
    post_pct: u8,
    // Per-hit base power override for multi-hit moves.
    bp_override: Option<u16>,
    // True for Gyro Ball / Electro Ball — defender's speed affects BP.
    speed_dep_bp: bool,
) {
    use crate::information::materialize::materialize_pokemon;

    // S11 soundness fix: Direction A materializes the attacker from its CURRENT
    // stat/item/ability fields as if they were the exact truth (`atk_stats =
    // attacker_unk.min_stats`, `neutral_item`/`neutral_ability`) — sound only for the
    // observer's own fully-`Known` Pokémon. Direction A fires whenever the target's
    // HP is `Percent`, which in doubles also covers an opponent mon hitting its OWN
    // ally with a spread move (the ally's HP is `Percent` too, since it belongs to
    // the non-observer side) — there the attacker is itself unknown, and treating
    // its unresolved stat bounds as exact would produce an unsound defender-BSV
    // bound. P1 is the observer throughout this module (see S16); only P1's moves
    // have a fully-Known attacker, so gate Direction A on that.
    //
    // Increment 2 note: this gate is also what guarantees the attacker here NEVER
    // carries a `possible_illusion_state` (only the non-viewer side is ever seeded
    // one) — so, unlike Direction B, Direction A's hypothesis mirror below doesn't
    // need a "both sides ambiguous" guard at all; it's provably unreachable.
    if user_slot.player != Player::P1 {
        return;
    }

    // S24: source both mons from the pre-move snapshots (see pass3_direction_b).
    // The defender's snapshot also carries a pre-move clone of its
    // `possible_illusion_state`, read directly by the hypothesis mirror below.
    let defender_unk = ctx
        .move_context
        .as_ref()
        .and_then(|mc| {
            mc.pre_move_targets
                .iter()
                .find(|(slot, _)| slot == target_slot)
                .map(|(_, m)| m.clone())
        })
        .or_else(|| get_mon_by_idx(state, target_idx).cloned());
    let Some(mut defender_unk) = defender_unk else {
        return;
    };
    // S23: materialize the defender at the HP this hit was actually taken at, so
    // full-HP-gated reducers (Multiscale / Shadow Shield / Tera Shell) stay live for
    // the hit that broke full HP.
    defender_unk.hp = hit_pre_hp.clone();
    // S26: a Transformed defender's Def/SpD are copied, not derived from its species
    // base — skip the BSV inversion (see pass3_direction_b).
    if defender_unk.pre_transform.is_some() {
        return;
    }
    let attacker_unk = ctx
        .move_context
        .as_ref()
        .and_then(|mc| mc.pre_move_attacker.clone())
        .or_else(|| get_mon_by_idx(state, user_idx).cloned());
    let Some(attacker_unk) = attacker_unk else {
        return;
    };

    // Attacker is OUR known mon; use its actual known stats. Materialized ONCE and
    // shared by both the primary defender AND (Increment 2) a mirrored hypothesis
    // search — the attacker is never ambiguous (see the S11 note above), so there is
    // nothing to recompute per-hypothesis here.
    let atk_stats = attacker_unk.min_stats;
    let atk_item = neutral_item(&attacker_unk);
    // Analytic correction: ×1.3 only when the attacker (our own mon = user_slot)
    // moves LAST this turn. When it did not fire, substitute Ability::None so the
    // oracle uses ×1.0 — otherwise the inflated damage prediction raises the
    // defender's min stat bound above the truth (unsound exclusion). S28: decided by
    // the precomputed per-segment last-mover, not by whether the target has moved.
    let atk_ability = {
        let raw = neutral_ability(&attacker_unk);
        if raw == Ability::Analytic && !analytic_fired(ctx, user_slot) {
            Ability::None
        } else {
            raw
        }
    };
    let atk_ps = materialize_pokemon(&attacker_unk, atk_stats, atk_item, atk_ability);

    if let Some((global_bsv_lo, global_bsv_hi, global_stat_lo, global_stat_hi, per_class_a, reducer_items, reducer_abilities, si)) =
        compute_defender_stat_bounds(
            state, &defender_unk, &atk_ps, user_slot, target_slot, move_data, def_stat,
            ctx, is_crit, pre_pct, post_pct, bp_override, speed_dep_bp,
        )
    {
        // Apply unconditional tightening.
        apply_unconditional_tightening(
            state, target_idx, si,
            global_bsv_lo, global_bsv_hi, global_stat_lo, global_stat_hi,
        );

        // ── I1: Conditional CNF predicates (Direction A) ─────────────────────
        // For each nature class κ, emit nature-guarded GE/LE clauses with reducer
        // disjuncts. Reducers (Eviolite, AV, Multiscale, …) could allow a lower raw
        // BSV to explain the observed damage, so they appear as disjuncts mirroring
        // Direction B's booster role. `current_pre_min/max` come from the
        // pre-tightening clone (defender_unk).
        emit_nature_conditional_bounds(
            state, target_idx, def_stat,
            &per_class_a, &reducer_items, &reducer_abilities,
            defender_unk.min_pre_nature_stat[si],
            defender_unk.max_pre_nature_stat[si],
        );
    }

    // Increment 2: mirror the same search onto a live Zoroark hypothesis, if the
    // defender's pre-move snapshot carried one. Never emits CNF (no sound `mon_idx`
    // to key a hypothesis's clause to) — see the plan's scope note. Note: only the
    // defensive stat is mirrored here, matching exactly what the primary path above
    // tightens — Direction A does not itself narrow HP (that would need a
    // `PokemonStat::Hp` variant, which doesn't exist; `hp_candidates` above is
    // enumeration input, not an output written back).
    if let Some(hyp) = defender_unk.possible_illusion_state.clone() {
        mirror_pass3_direction_a_onto_hypothesis(
            state, target_idx, *hyp, &atk_ps, user_slot, target_slot, move_data,
            def_stat, ctx, is_crit, pre_pct, post_pct, bp_override, speed_dep_bp,
        );
    }
}

/// Core Direction A search: for a candidate defender (`defender_unk`) that took the
/// observed pre/post-hit display percents from `atk_ps`'s hit with `move_data`,
/// enumerates achievable HP hypotheses × nature classes × defensive items ×
/// abilities, binary-searching the feasible BSV interval via
/// `find_feasible_bsv_range_a` for each combination and unioning into a single
/// tightest-known window. Extracted verbatim from `pass3_direction_a` so the SAME
/// search can run against either the real defender (primary) or a live Zoroark
/// hypothesis (Increment 2).
///
/// Pure computation over its value parameters — `atk_ps` (the attacker, already
/// materialized, always the viewer's own unambiguous mon) is a FIXED oracle input,
/// never re-derived per hypothesis.
///
/// Returns `None` for the same early-return conditions the primary path always had
/// (unknown defender species, empty pre-nature window, no possible nature class) —
/// treat as "no new evidence from this hit," never a contradiction. Returns
/// `Some((global_bsv_lo, global_bsv_hi, global_stat_lo, global_stat_hi, per_class_a,
/// reducer_items, reducer_abilities, si))` otherwise.
#[allow(clippy::too_many_arguments)]
fn compute_defender_stat_bounds(
    state: &UnknownBattleState,
    defender_unk: &UnknownPokemonState,
    atk_ps: &crate::state::pokemon::PokemonState,
    user_slot: &FieldSlot,
    target_slot: &FieldSlot,
    move_data: &crate::state::dex_data::MoveData,
    def_stat: &crate::state::dex_data::PokemonStat,
    ctx: &BattleContext,
    is_crit: bool,
    pre_pct: u8,
    post_pct: u8,
    bp_override: Option<u16>,
    speed_dep_bp: bool,
) -> StatBoundsSearchResult {
    use crate::simulator::DamageConfig;

    // Need known defender species for BSV inference.
    let base_stats = match &defender_unk.possible_species {
        Unknown::Known(s) => match ctx.dex.get(s) {
            Some(d) => d.base_stats,
            None => return None,
        },
        _ => return None,
    };

    let si = bcp::stat_to_stats_idx(def_stat);
    let level = defender_unk.level;

    let bsv_lo = defender_unk.min_pre_nature_stat[si];
    let bsv_hi = defender_unk.max_pre_nature_stat[si];
    if bsv_lo > bsv_hi {
        return None;
    }

    // Nature classes for the defensive stat.
    let nature_classes = possible_nature_classes(&defender_unk.possible_natures, def_stat, si);
    if nature_classes.is_empty() {
        return None;
    }

    // Oracle config.
    let oracle_config = DamageConfig {
        consider_crit: true,
        damage_rolls: 16,
        sample: false,
    };
    // Spread multiplier: ×0.75 in doubles when the move targets all adjacent foes.
    // Omitting this caused the back-solved defensive BSV to be off by 1/0.75 for
    // spread moves in doubles (S2).
    let targets_mult = spread_targets_mult(state, move_data);

    // ── Unconditional tightening: union over (nat, hp_candidate, def_bsv, def_item, def_ability) ────
    // Also accumulates per-nature-class neutral-gear bounds used by I1 predicate emission.
    let mut global_bsv_lo: Option<u16> = None;
    let mut global_bsv_hi: Option<u16> = None;
    let mut global_stat_lo: Option<u16> = None;
    let mut global_stat_hi: Option<u16> = None;

    // Defender speed endpoints for Gyro Ball / Electro Ball (Direction A: we attacked
    // the opponent, so the *target/defender's* speed is the unknown that affects BP).
    // BP is monotone in the speed ratio, so scanning only the lo/hi endpoints is sound.
    let defender_speed_endpoints: Vec<u16> = if speed_dep_bp {
        let lo = defender_unk.min_stats[5];
        let hi = defender_unk.max_stats[5];
        if lo == hi { vec![lo] } else { vec![lo, hi] }
    } else {
        vec![defender_unk.min_stats[5]]
    };

    // S1 soundness fix: union over defender's possible (item, ability) pairs so we
    // never raise min_pre_nature_stat above the truth for a bulk-item/resistance-ability
    // defender.  Mirrors how Direction B already unions over offensive items/abilities.
    //
    // E-B optimisation: prune entries that are provably inert for this specific move
    // (wrong type, wrong category, wrong flag).  Sound: each rule keeps the entry
    // whenever there is any doubt.
    // atk_ability was moved into materialize_pokemon; read from atk_ps which holds the same value.
    let eff_move_type = pruning_move_type(&atk_ps.ability, move_data);
    let def_items = {
        let mut items = defensive_damage_items(defender_unk);
        items.retain(|item| {
            // AssaultVest: stat-bake handled in run_def_oracle (×1.5 SpD Special only).
            // For Physical moves it contributes nothing — identical to neutral item run.
            if *item == Item::AssaultVest
                && matches!(move_data.category, MoveCategory::Physical) {
                return false;
            }
            // Type-resist berries only trigger when the berry's type matches the move.
            if let Some(berry_type) = type_resist_berry_type(item) {
                return berry_type == eff_move_type;
            }
            true
        });
        items
    };
    let def_abilities = {
        let mut abilities = defensive_damage_abilities(defender_unk);
        abilities.retain(|ability| {
            match ability {
                // Category-gated.
                Ability::IceScales => matches!(move_data.category, MoveCategory::Special),
                Ability::FurCoat   => matches!(move_data.category, MoveCategory::Physical),
                // Type-gated.
                Ability::Heatproof    => matches!(eff_move_type, PokemonType::Fire),
                Ability::WaterBubble  => matches!(eff_move_type, PokemonType::Fire),
                Ability::ThickFat     => matches!(eff_move_type, PokemonType::Fire | PokemonType::Ice),
                Ability::PurifyingSalt => matches!(eff_move_type, PokemonType::Ghost),
                Ability::FairyAura    => matches!(eff_move_type, PokemonType::Fairy),
                Ability::DrySkin      => matches!(eff_move_type, PokemonType::Fire),
                // Flag-gated.
                Ability::PunkRock => move_has_flag(move_data, &MoveFlag::Sound),
                // Fluffy: ×0.5 to contact, ×2 to Fire — prune only if neither.
                Ability::Fluffy => {
                    move_has_flag(move_data, &MoveFlag::Contact)
                        || matches!(eff_move_type, PokemonType::Fire)
                }
                // SE-gated (Filter/SolidRock/PrismArmor: ×0.75 on SE hits only).
                // Prune if we can confirm the move is not SE; keep when types unknown.
                Ability::Filter | Ability::SolidRock | Ability::PrismArmor => {
                    if let Unknown::Known(def_types) = &defender_unk.possible_types {
                        let eff = def_types.iter().fold(1.0_f64, |acc, t| {
                            acc * single_type_effectiveness(&eff_move_type, t)
                        });
                        eff > 1.0 // keep only when SE
                    } else {
                        true // unknown types → keep
                    }
                }
                // HP-gated (Multiscale/ShadowShield/TeraShell) or always-relevant: never prune.
                _ => true,
            }
        });
        abilities
    };

    let neutral_def_item = neutral_item(defender_unk);
    let neutral_def_ability = neutral_ability(defender_unk);

    // Per-nature-class neutral-gear BSV bounds, accumulated across hp_cand hypotheses.
    // Used by the I1 CNF predicate emission after the main loops.
    // bsv_lo_neutral: min over hp_cands (widest/most-conservative lower bound)
    // bsv_hi_neutral: max over hp_cands
    let mut per_class_a: Vec<NatureClassBound> = nature_classes
        .iter()
        .map(|&(m, b, n)| NatureClassBound {
            mod_f32: m, is_boost: b, is_nerf: n,
            bsv_lo_neutral: None, bsv_hi_neutral: None,
        })
        .collect();

    // Enumerate exactly the achievable HP values for this defender (S-B soundness fix).
    // A stride-4 sample can skip achievable HP values whose feasible-BSV interval
    // lies outside the sampled union, causing min_pre_nature_stat to be raised
    // above the true value (unsound exclusion).  Using the EV-lattice enumeration
    // ensures every realistically achievable HP is covered.
    let hp_candidates =
        achievable_defender_hp_values(base_stats[0], level, ctx.config, defender_unk);
    for hp_cand in hp_candidates {
        // S22: exact damage band for this max-HP hypothesis from the display buckets
        // of BOTH endpoints (each percent carries its own rounding). A `None` band
        // means this hp_cand cannot display the observed percents at all — skip it.
        let Some((d_lo, d_hi)) = percent_delta_damage_band(pre_pct, post_pct, hp_cand) else {
            continue;
        };

        for (class_idx, (nat_mod, _is_boost, _is_nerf)) in nature_classes.iter().enumerate() {
            // Thin wrapper so the two call sites below don't repeat the full argument list.
            // Mirrors the shape of Direction B's find_feasible_bsv_range_b calls.
            let search = |di: &Item, da: &Ability| {
                find_feasible_bsv_range_a(
                    state, defender_unk, atk_ps,
                    user_slot, target_slot, move_data,
                    oracle_config, targets_mult, *nat_mod, si, bsv_lo, bsv_hi,
                    hp_cand, di, da, &defender_speed_endpoints,
                    d_lo, d_hi, is_crit, bp_override,
                )
            };

            // Neutral-gear bounds (for I1 predicate emission): union across hp_cands.
            // min(bsv_lo) gives the widest/most-conservative lower bound across all HP hypotheses.
            let (neutral_lo, neutral_hi) = search(&neutral_def_item, &neutral_def_ability);
            {
                let cr = &mut per_class_a[class_idx];
                if let Some(lo) = neutral_lo {
                    cr.bsv_lo_neutral = Some(cr.bsv_lo_neutral.map_or(lo, |g: u16| g.min(lo)));
                }
                if let Some(hi) = neutral_hi {
                    cr.bsv_hi_neutral = Some(cr.bsv_hi_neutral.map_or(hi, |g: u16| g.max(hi)));
                }
            }

            // Full union over all (def_item, def_ability) combos for unconditional tightening.
            let mut found_lo_local: Option<u16> = None;
            let mut found_hi_local: Option<u16> = None;
            for def_item in &def_items {
                for def_ability in &def_abilities {
                    // Reuse the already-computed neutral-gear result when we hit that combo;
                    // find_feasible_bsv_range_a runs a bracketed binary search — not free.
                    let (lo, hi) = if def_item == &neutral_def_item && def_ability == &neutral_def_ability {
                        (neutral_lo, neutral_hi)
                    } else {
                        search(def_item, def_ability)
                    };
                    if let (Some(lo_v), Some(hi_v)) = (lo, hi) {
                        found_lo_local = Some(found_lo_local.map_or(lo_v, |g: u16| g.min(lo_v)));
                        found_hi_local = Some(found_hi_local.map_or(hi_v, |g: u16| g.max(hi_v)));
                    }
                }
            }
            if let (Some(lo_v), Some(hi_v)) = (found_lo_local, found_hi_local) {
                let nat_mod = nature_classes[class_idx].0;
                let final_lo = (lo_v as f64 * nat_mod as f64).floor() as u16;
                let final_hi = (hi_v as f64 * nat_mod as f64).floor() as u16;
                global_bsv_lo = Some(global_bsv_lo.map_or(lo_v, |g| g.min(lo_v)));
                global_bsv_hi = Some(global_bsv_hi.map_or(hi_v, |g| g.max(hi_v)));
                global_stat_lo = Some(global_stat_lo.map_or(final_lo, |g| g.min(final_lo)));
                global_stat_hi = Some(global_stat_hi.map_or(final_hi, |g| g.max(final_hi)));
            }
        }
    }

    let reducer_items: Vec<Item> = def_items
        .iter()
        .filter(|i| **i != neutral_def_item)
        .cloned()
        .collect();
    let reducer_abilities: Vec<Ability> = def_abilities
        .iter()
        .filter(|a| **a != neutral_def_ability)
        .cloned()
        .collect();

    Some((
        global_bsv_lo, global_bsv_hi, global_stat_lo, global_stat_hi,
        per_class_a, reducer_items, reducer_abilities, si,
    ))
}

/// Mirrors Direction A's defensive-stat tightening onto a live Zoroark hypothesis
/// (Increment 2). `hyp` is the defender's pre-move hypothesis snapshot (from
/// `defender_unk`'s own cloned `possible_illusion_state`). No "both sides ambiguous"
/// guard is needed here (unlike Direction B) — the attacker is provably never
/// ambiguous under the S11 P1-attacker-only gate. Never emits CNF; detects
/// infeasibility via an inline inverted-window check, since Pass 3 itself never
/// panics.
#[allow(clippy::too_many_arguments)]
fn mirror_pass3_direction_a_onto_hypothesis(
    state: &mut UnknownBattleState,
    target_idx: usize,
    mut hyp: UnknownPokemonState,
    atk_ps: &crate::state::pokemon::PokemonState,
    user_slot: &FieldSlot,
    target_slot: &FieldSlot,
    move_data: &crate::state::dex_data::MoveData,
    def_stat: &crate::state::dex_data::PokemonStat,
    ctx: &BattleContext,
    is_crit: bool,
    pre_pct: u8,
    post_pct: u8,
    bp_override: Option<u16>,
    speed_dep_bp: bool,
) {
    // S26 (mirrored): a Transformed hypothesis can't be soundly analyzed this way.
    if hyp.pre_transform.is_some() {
        write_back_pass3_hypothesis(state, target_idx, Some(Box::new(hyp)));
        return;
    }

    let feasible = match compute_defender_stat_bounds(
        state, &hyp, atk_ps, user_slot, target_slot, move_data, def_stat,
        ctx, is_crit, pre_pct, post_pct, bp_override, speed_dep_bp,
    ) {
        None => true, // no new evidence derivable from this hit for this identity
        Some((bsv_lo, bsv_hi, stat_lo, stat_hi, _per_class, _items, _abilities, si)) => {
            apply_unconditional_tightening_to_mon(&mut hyp, si, bsv_lo, bsv_hi, stat_lo, stat_hi);
            hyp.min_pre_nature_stat[si] <= hyp.max_pre_nature_stat[si]
                && hyp.min_stats[si] <= hyp.max_stats[si]
        }
    };

    write_back_pass3_hypothesis(state, target_idx, feasible.then_some(Box::new(hyp)));
}

// ── Pass 4: Speed ordering → Spe bounds ──────────────────────────────────────

/// Returns the `mon_idx` of P2's first active slot (P2's active segment immediately
/// follows P1's active segment under the S1 layout — see `MonSegments`).
fn p2_mon_start(state: &UnknownBattleState) -> usize {
    state.p1_active_mons.len()
}

/// `true` if `mon_idx` belongs to a P2 roster member (active or benched). S1: P2's
/// bench segment is no longer contiguous with P2's active segment (P1's bench sits
/// between them), so this checks both P2 ranges explicitly rather than a single
/// ">=" boundary.
fn mon_is_p2(state: &UnknownBattleState, mon_idx: usize) -> bool {
    let [_, p2_active, _, p2_back] = MonSegments::of(state).ranges();
    p2_active.contains(&mon_idx) || p2_back.contains(&mon_idx)
}

/// `(item, abilities, known_types)` snapshot returned by [`snapshot_item_ability_type`].
type ItemAbilityTypeSnapshot = (
    Option<Unknown<Item>>,
    Option<Unknown<Ability>>,
    Option<Vec<PokemonType>>,
);

/// Extract the core item/ability/type snapshot from a target mon into owned values.
/// Callers need owned copies to avoid borrow conflicts when later mutating `state`.
fn snapshot_item_ability_type(
    state: &UnknownBattleState,
    mon_idx: usize,
) -> ItemAbilityTypeSnapshot {
    let tm = get_mon_by_idx(state, mon_idx);
    let tm_item = tm.map(|m| m.item.clone());
    let tm_abilities = tm.map(|m| m.possible_abilities.clone());
    let known_types = tm.and_then(|m| {
        if let Unknown::Known(ts) = &m.possible_types { Some(ts.clone()) } else { None }
    });
    (tm_item, tm_abilities, known_types)
}

/// Returns the effective move priority for `move_used`, folding in field-conditional
/// boosts that are deterministically known from state (Grassy Glide +1 on
/// Grassy Terrain).  Does NOT fold in ability-based boosts (Prankster/Gale Wings/
/// Triage); those are folded in by callers that have access to move data and user state.
/// S32: takes the terrain EXPLICITLY (the per-move-scan snapshot from
/// `pass4_speed_from_order`, reflecting terrain as of just before this move — see
/// its doc comment) rather than reading `state.terrain` live, for the same
/// mid-turn/end-of-turn staleness reason as `compute_speed_multipliers` (S4):
/// Grassy Terrain can be set or expire mid-turn or by end of turn, and reading it
/// live at Pass 4's (second, post-walk) call time can disagree with what actually
/// determined the observed priority bracket.
fn effective_move_priority(
    move_used: &PokemonMove,
    base_priority: i8,
    terrain: &Option<Terrain>,
) -> i8 {
    if *move_used == PokemonMove::GrassyGlide
        && *terrain == Some(Terrain::GrassyTerrain)
    {
        base_priority + 1
    } else {
        base_priority
    }
}

/// Adjust a move's effective priority by any **Known** priority-lifting ability on the user.
/// Only fires when the ability is `Known(X)` — `Possibly` leaves the escape disjunct path.
fn fold_known_ability_priority(
    move_data: &MoveData,
    base_prio: i8,
    user_mon: &crate::information::unknowns::UnknownPokemonState,
) -> i8 {
    let Unknown::Known(ab) = &user_mon.possible_abilities else {
        return base_prio;
    };
    match ab {
        Ability::Prankster if move_data.category == MoveCategory::Status => base_prio + 1,
        Ability::GaleWings
            if move_data.pokemon_type == PokemonType::Flying
                && matches!(user_mon.hp, PokemonHP::Percent(100)) =>
        {
            base_prio + 1
        }
        Ability::Triage if move_data.heal_fraction != [0, 0] => base_prio + 3,
        _ => base_prio,
    }
}

/// Escape `Statement`s that explain a move going first despite having lower declared
/// priority (cross-bracket) or being the natural speed-loser (same-bracket).
///
/// Returns: Prankster (iff Status category), Gale Wings (iff Flying + full HP), Triage
/// (iff `heal_fraction != [0,0]`), Quick Claw (item), Quick Draw (ability) — each
/// gated on `!unknown_is_excluded` so no extraneous clause entries are added for
/// abilities/items already ruled out.
///
/// Used by both the **cross-bracket** path (where these are the *entire* clause) and the
/// **same-bracket** path (where they are prepended to the longer speed-tweak list), so
/// any future change to these triggers (e.g. Gale Wings full-HP condition) only needs
/// one edit here.
fn priority_lift_escapes(
    state: &UnknownBattleState,
    fast_idx: usize,
    fast_move: &PokemonMove,
    move_dex: &HashMap<PokemonMove, MoveData>,
) -> Vec<Statement> {
    let (Some(fast_m), Some(fast_md)) =
        (get_mon_by_idx(state, fast_idx), move_dex.get(fast_move))
    else {
        return vec![];
    };

    let mut escapes: Vec<Statement> = Vec::new();

    // Prankster: +1 to Status-category moves.
    if fast_md.category == MoveCategory::Status
        && !unknown_is_excluded(&fast_m.possible_abilities, &Ability::Prankster)
    {
        escapes.push(Statement::HasAbility { mon_idx: fast_idx, ability: Ability::Prankster });
    }
    // Gale Wings: +1 to Flying-type moves at full HP (Gen VIII+ condition).
    let fast_at_full_hp = matches!(fast_m.hp, PokemonHP::Percent(100));
    if fast_md.pokemon_type == PokemonType::Flying
        && fast_at_full_hp
        && !unknown_is_excluded(&fast_m.possible_abilities, &Ability::GaleWings)
    {
        escapes.push(Statement::HasAbility { mon_idx: fast_idx, ability: Ability::GaleWings });
    }
    // Triage: +3 to draining/healing moves.
    if fast_md.heal_fraction != [0, 0]
        && !unknown_is_excluded(&fast_m.possible_abilities, &Ability::Triage)
    {
        escapes.push(Statement::HasAbility { mon_idx: fast_idx, ability: Ability::Triage });
    }
    // Quick Claw / Quick Draw (random first-mover item / ability).
    if !unknown_is_excluded(&fast_m.item, &Item::QuickClaw) {
        escapes.push(Statement::HasItem { mon_idx: fast_idx, item: Item::QuickClaw });
    }
    if !unknown_is_excluded(&fast_m.possible_abilities, &Ability::QuickDraw) {
        escapes.push(Statement::HasAbility { mon_idx: fast_idx, ability: Ability::QuickDraw });
    }

    escapes
}

/// Per-mover snapshot used by `pass4_speed_from_order`: everything needed to emit a
/// pairing's clause, including the speed-relevant fields (Spe boost stage, paralysis,
/// Tailwind, Trick Room, weather, terrain) captured AS OF the point in the turn just
/// before this mover's own `MoveUsed`, not read live from `state` at Pass 4's call
/// time (see S4 comment on `compute_speed_multipliers`, and S32 below).
struct Mover {
    eff_prio: i8,
    mon_idx: usize,
    /// The side this mover's own mon is on — needed by S36's same-side tailwind fix
    /// below to look up the RIGHT side's flag from another mover's snapshot.
    player: Player,
    move_used: PokemonMove,
    spe_boost: i8,
    paralyzed: bool,
    /// S36: BOTH sides' Tailwind state as of just before this mover acted (not just
    /// this mover's own side) — see the windows(2) loop in `pass4_speed_from_order`
    /// for why a same-side pairing needs the EARLIER mover's snapshot for BOTH
    /// participants, not each participant's own separate snapshot time.
    p1_tailwind: bool,
    p2_tailwind: bool,
    /// S32: Trick Room state as of just before this mover acted. Trick Room can be
    /// set or removed mid-turn (e.g. a Trick Room use, or a Room Service/Room-ending
    /// move); a single global read at Pass 4's call time — especially the *second*
    /// call, which runs after the event walk has mutated `state` to end-of-turn field
    /// conditions — would misattribute a later Trick Room flip to earlier pairings.
    trick_room: bool,
    /// S32: weather as of just before this mover acted, for the weather-ability escape
    /// disjuncts (Swift Swim/Chlorophyll/Sand Rush/Slush Rush) — same rationale as
    /// `trick_room`.
    weather: Option<Weather>,
    /// S32: terrain as of just before this mover acted, for the Surge Surfer escape.
    terrain: Option<Terrain>,
}

/// Deep-scan `reactions` for events that change a mon's Spe boost stage, paralysis
/// status, a side's Tailwind, Trick Room, weather, or terrain — the fields
/// `compute_speed_multipliers` and the weather/TR escape logic in
/// `pass4_speed_from_order` bake into a `SpeedComparison`'s numeric factors and
/// escape disjuncts — and update the running snapshot state (S32).
///
/// Deliberately narrow: `BoostsSwapped`/`BoostsCopied` are not tracked (which
/// specific stats they move isn't recoverable from the event alone without the
/// causing move; Heart Swap/Power Swap/Guard Swap mid-turn before a same-turn
/// speed-relevant pairing is rare enough that leaving the snapshot stale here is an
/// acceptable, documented residual gap rather than blocking the fix for the common
/// cases (Thunder Wave, Icy Wind/Charm, Intimidate-adjacent, Tailwind, Haze).
#[allow(clippy::too_many_arguments)]
fn update_speed_snapshot_from_reactions(
    state: &UnknownBattleState,
    reactions: &[InformationEvent],
    spe_boost: &mut HashMap<usize, i8>,
    paralyzed: &mut HashMap<usize, bool>,
    tailwind: &mut HashMap<Player, bool>,
    trick_room: &mut bool,
    weather: &mut Option<Weather>,
    terrain: &mut Option<Terrain>,
) {
    for r in reactions {
        match &r.kind {
            EventKind::StatusInflicted { target, status } => {
                if let Some(idx) = mon_idx_for_active_slot(state, target) {
                    paralyzed.insert(idx, matches!(status, Status::Paralysis));
                }
            }
            EventKind::StatusCured { target, .. } => {
                if let Some(idx) = mon_idx_for_active_slot(state, target) {
                    paralyzed.insert(idx, false);
                }
            }
            EventKind::BoostChanged { target, boost_idx: 4, stages } => {
                if let Some(idx) = mon_idx_for_active_slot(state, target) {
                    let cur = *spe_boost.get(&idx).unwrap_or(&0);
                    spe_boost.insert(idx, (cur as i16 + *stages as i16).clamp(-6, 6) as i8);
                }
            }
            EventKind::BoostsCleared { target } => {
                if let Some(idx) = mon_idx_for_active_slot(state, target) {
                    spe_boost.insert(idx, 0);
                }
            }
            EventKind::BoostsInverted { target } => {
                if let Some(idx) = mon_idx_for_active_slot(state, target) {
                    let cur = *spe_boost.get(&idx).unwrap_or(&0);
                    spe_boost.insert(idx, -cur);
                }
            }
            EventKind::SideConditionStart { side, condition: SideCondition::TailWind } => {
                tailwind.insert(*side, true);
            }
            EventKind::SideConditionEnd { side, condition: SideCondition::TailWind } => {
                tailwind.insert(*side, false);
            }
            EventKind::PseudoWeatherStart { effect: PseudoWeather::TrickRoom } => {
                *trick_room = true;
            }
            EventKind::PseudoWeatherEnd { effect: PseudoWeather::TrickRoom } => {
                *trick_room = false;
            }
            EventKind::WeatherChanged { weather: w } => {
                *weather = w.clone();
            }
            EventKind::TerrainChanged { terrain: t } => {
                *terrain = t.clone();
            }
            _ => {}
        }
        update_speed_snapshot_from_reactions(
            state, &r.reactions, spe_boost, paralyzed, tailwind, trick_room, weather, terrain,
        );
    }
}

/// Emit `SpeedComparison` predicates from the observed top-level move order.
///
/// For each pair of consecutive moves in the same effective priority bracket:
/// - Wraps the natural SpeedComparison in a disjunction with any move-order explanation
///   that could account for the ordering without implying a speed edge (Quick Claw,
///   Quick Draw, ability priority, Stall, item speed modifiers, weather abilities, etc.).
/// - Accounts for Trick Room (reverses the inferred fast/slow assignment) and Tailwind
///   (folds the ×2 multiplier into the comparison deterministically).
///
/// S32: `seed_state` is a clone of `UnknownBattleState` taken *before* this turn's
/// event walk (`apply_information_battle`, before `process_battle_event` runs).
/// `pass4_speed_from_order` is invoked twice — once before the walk (to tighten Spe
/// bounds ahead of Pass 3's damage oracle) and once after (to pick up any
/// priority-lifting ability BCP forced to Known mid-walk). The *initial* seed for
/// every running speed-relevant tracker (Tailwind, Trick Room, weather, terrain,
/// per-mon Spe boost/paralysis) must always be `seed_state` — i.e. what was true at
/// turn start — never `state` at call time, because on the second call `state` has
/// already been mutated to end-of-turn field conditions by the walk. Reading `state`
/// there previously attributed a Tailwind/Trick Room/weather change from later in
/// the turn (or from end-of-turn expiry) to pairings that raced before it existed —
/// the root cause of spurious `SpeedComparison raises min above max` contradictions
/// tagged with a misleading `event=EndOfTurn`/`event=VolatileEnd` breadcrumb (that
/// breadcrumb is just whatever event the prior event-walk pass last visited; Pass 4
/// itself never reads `VolatileEnd`/`EndOfTurn` — see `CURRENT_EVENT_CONTEXT`).
/// `state` itself is still used for everything NOT speed-relevant-field state (mon
/// lookups by idx, writing the emitted predicates) since those are structural /
/// output, not turn-start-vs-live field values.
fn pass4_speed_from_order(
    state: &mut UnknownBattleState,
    seed_state: &UnknownBattleState,
    top_events: &[InformationEvent],
    dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    _ability_dex: &HashMap<Ability, AbilityData>,
) {
    // Running snapshot of Spe boost / paralysis / Tailwind / Trick Room / weather /
    // terrain, seeded from `seed_state` (turn start) and updated as we scan forward
    // through `top_events` — see `Mover` and `update_speed_snapshot_from_reactions`
    // (S4, S32).
    let mut spe_boost: HashMap<usize, i8> = HashMap::new();
    let mut paralyzed: HashMap<usize, bool> = HashMap::new();
    let mut tailwind: HashMap<Player, bool> = HashMap::new();
    tailwind.insert(Player::P1, seed_state.p1_side_conditions.contains(&SideCondition::TailWind));
    tailwind.insert(Player::P2, seed_state.p2_side_conditions.contains(&SideCondition::TailWind));
    let mut trick_room = seed_state.pseudo_weathers.contains(&PseudoWeather::TrickRoom);
    let mut weather = seed_state.weather.clone();
    let mut terrain = seed_state.terrain.clone();

    // Collect one Mover per top-level MoveUsed event, each carrying a snapshot taken
    // AS OF the point in the scan just before its own MoveUsed — i.e. reflecting
    // every earlier top-level event's effects (including this event's own
    // reactions are applied AFTER recording, so a later mover sees them, not this
    // one).
    let mut move_order: Vec<Mover> = Vec::new();
    for event in top_events {
        // S41: Mega Evolution / permanent Forme Change swap in a new base-stat table
        // and always resolve before any move that turn (real game rule) — but this is
        // the FIRST of pass4's two calls, which runs BEFORE the real event walk (see
        // the doc comment above), so `state` has NOT yet been mutated by the walk's own
        // `MegaEvolution`/`FormeChange` handler. Scanning `top_events` in order without
        // applying this ourselves means every `Mover` built for the REST of this same
        // scan — including the mega'd mon's own `MoveUsed`, later in this very list —
        // would read its PRE-mega Speed, even though by the time it actually moved that
        // turn its Speed was already post-mega. Apply the same recompute the real
        // handler uses, directly to `state`, so later Movers in this scan see the
        // correct value. Idempotent: the real walk applies the identical overwrite
        // again later for real, to the same target values.
        if let EventKind::MegaEvolution { slot, into } | EventKind::FormeChange { slot, into, .. } = &event.kind
            && let Some(idx) = mon_idx_for_active_slot(state, slot) {
                if let (Some(mon), Some(data)) = (get_mon_mut_by_idx(state, idx), dex.get(into)) {
                    mon.possible_species = Unknown::Known(into.clone());
                    recompute_stat_bounds_for_species_change(mon, data.base_stats, mon.level);
                }
                // Same purge the real walk's MegaEvolution/FormeChange handler performs
                // (`statement_stale_after_species_reveal`) — applied here too, since this
                // early recompute now makes a pre-existing SpeedComparison/EVIVStatGE/LE
                // clause derived against the OLD base-stat table stale immediately, not
                // just once the real walk gets around to processing this same event.
                // Without this, the very next line's synchronous `propagate_collected`
                // can apply a stale pre-mega cap to the freshly-widened bound and panic
                // before the real walk ever runs.
                state.predicates.retain(|clause| {
                    !clause
                        .iter()
                        .any(|lit| statement_stale_after_species_reveal(lit, idx))
                });
            }
        if let EventKind::MoveUsed {
            user, move_used, ..
        } = &event.kind
        {
            let base_prio = move_dex.get(move_used).map(|md| md.priority).unwrap_or(0);
            let mut eff_prio = effective_move_priority(move_used, base_prio, &terrain);
            if let Some(idx) = mon_idx_for_active_slot(state, user) {
                // Fold in Known priority-lifting abilities to get the tightest bracket.
                if let (Some(mon), Some(md)) = (get_mon_by_idx(state, idx), move_dex.get(move_used)) {
                    eff_prio = fold_known_ability_priority(md, eff_prio, mon);
                }
                let s_boost = *spe_boost.entry(idx).or_insert_with(|| {
                    get_mon_by_idx(seed_state, idx).map_or(0, |m| m.boosts[4])
                });
                let s_para = *paralyzed.entry(idx).or_insert_with(|| {
                    get_mon_by_idx(seed_state, idx)
                        .is_some_and(|m| matches!(m.status, Some(Status::Paralysis)))
                });
                tailwind.entry(user.player).or_insert(false);
                let p1_tw = *tailwind.get(&Player::P1).unwrap_or(&false);
                let p2_tw = *tailwind.get(&Player::P2).unwrap_or(&false);
                move_order.push(Mover {
                    eff_prio,
                    mon_idx: idx,
                    player: user.player,
                    move_used: move_used.clone(),
                    spe_boost: s_boost,
                    paralyzed: s_para,
                    p1_tailwind: p1_tw,
                    p2_tailwind: p2_tw,
                    trick_room,
                    weather: weather.clone(),
                    terrain: terrain.clone(),
                });
            }
        }
        update_speed_snapshot_from_reactions(
            state, &event.reactions, &mut spe_boost, &mut paralyzed, &mut tailwind,
            &mut trick_room, &mut weather, &mut terrain,
        );
    }

    for window in move_order.windows(2) {
        let mover0 = &window[0];
        let mover1 = &window[1];
        let (p0, idx0, mv0) = (mover0.eff_prio, mover0.mon_idx, &mover0.move_used);
        let (p1, idx1) = (mover1.eff_prio, mover1.mon_idx);

        // Different effective priority brackets.
        // If the first mover has a *lower* effective priority than the second (p0 < p1),
        // the observation is only explicable by a priority-lifting ability on the first
        // mover (Prankster, Gale Wings, Triage) or by a random first-mover effect.
        // Emit a disjunction for these; if it collapses to a unit clause BCP will force
        // the ability.  If p0 > p1, normal priority ordering — no inference possible.
        if p0 != p1 {
            if p0 < p1 {
                // Earlier mover had lower declared priority — must have a lifter.
                // The escapes are exactly the priority-lift abilities/items; no
                // SpeedComparison literal here since bracket ordering dominates speed.
                let fast_idx = idx0;
                let clause = priority_lift_escapes(state, fast_idx, mv0, move_dex);
                if !clause.is_empty() {
                    state.predicates.push(clause);
                }
            }
            // p0 > p1: normal priority ordering, no inference.
            continue;
        }

        // Under Trick Room the slower mon goes first; swap the fast/slow assignment
        // (and their snapshotted speed-relevant fields along with it). Uses mover0's
        // own snapshot of Trick Room (S32) — the field state as of just before the
        // FIRST of this pair acted, i.e. as of the moment this pairing's ordering was
        // actually determined — rather than a single global read at Pass 4's call time.
        let trick_room_active = mover0.trick_room;
        let (fast_idx, slow_idx, fast_move, fast_snap, slow_snap) = if trick_room_active {
            // mover1 went second → is the faster mon in normal ordering.
            (idx1, idx0, mover1.move_used.clone(), mover1, mover0)
        } else {
            (idx0, idx1, mv0.clone(), mover0, mover1)
        };

        // S36: Tailwind is looked up from `mover0` — the TEMPORALLY EARLIER mover,
        // always (Trick Room only relabels which one is "fast", it doesn't change
        // WHEN the comparison was actually decided) — for BOTH participants' sides,
        // not each mon's own separate snapshot time. The action-selection algorithm
        // that produced this observed order picked `mover0` as the winner from a pool
        // that included `mover1` AS IT STOOD AT THAT MOMENT; `mover1`'s own (necessarily
        // later-or-equal) snapshot can already reflect a side-wide field change `mover0`
        // itself just caused. This matters specifically for a same-side pair where the
        // earlier mover's own move is what changed the field (e.g. Aerodactyl casts
        // Tailwind and is immediately followed by its own teammate): the teammate's own
        // snapshot correctly shows Tailwind active (needed for the NEXT pairing, against
        // the opposing side), but that Tailwind boost must not be backed into THIS pairing
        // against Aerodactyl, which raced its teammate before Tailwind existed. Using
        // `mover1.tailwind` there previously required Aerodactyl's own raw Speed to be
        // at least double its teammate's — impossible once that teammate's real Speed
        // was high enough, producing exactly this class of "SpeedComparison raises min
        // above max" contradiction.
        let fast_tailwind = match fast_snap.player {
            Player::P1 => mover0.p1_tailwind,
            Player::P2 => mover0.p2_tailwind,
        };
        let slow_tailwind = match slow_snap.player {
            Player::P1 => mover0.p1_tailwind,
            Player::P2 => mover0.p2_tailwind,
        };
        let (fast_mult, slow_mult) = compute_speed_multipliers(
            fast_snap.spe_boost,
            slow_snap.spe_boost,
            fast_snap.paralyzed,
            slow_snap.paralyzed,
            fast_tailwind,
            slow_tailwind,
        );

        // ── Build escape disjuncts ────────────────────────────────────────────
        // Every escape disjunct D means: "the SpeedComparison OR the escape D explains
        // the observation" — so the predicate remains sound (a wider union).

        let fast_mon = get_mon_by_idx(state, fast_idx);
        let slow_mon = get_mon_by_idx(state, slow_idx);

        let mut clause: Vec<Statement> = vec![Statement::SpeedComparison {
            fast_idx,
            slow_idx,
            fast_mult,
            slow_mult,
        }];

        // (1)+(2) Priority-lift escapes: Quick Claw, Quick Draw, Prankster, Gale Wings, Triage.
        // Extracted from the cross-bracket path (where these are the *whole* clause) to
        // avoid the identical logic being maintained in two places.
        clause.extend(priority_lift_escapes(state, fast_idx, &fast_move, move_dex));

        // (3) Stall on the slow mon: Stall forces the holder to always go last
        //     within its priority bracket regardless of speed.
        if slow_mon.is_some_and(|m| {
            !unknown_is_excluded(&m.possible_abilities, &Ability::Stall)
        }) {
            clause.push(Statement::HasAbility {
                mon_idx: slow_idx,
                ability: Ability::Stall,
            });
        }

        // (4) Choice Scarf on the fast mon: ×1.5 effective speed means the natural
        //     SpeedComparison predicate is too strong (over-narrows) without this escape.
        if fast_mon.is_some_and(|m| !unknown_is_excluded(&m.item, &Item::ChoiceScarf)) {
            clause.push(Statement::HasItem {
                mon_idx: fast_idx,
                item: Item::ChoiceScarf,
            });
        }

        // (5) Speed-reducing items on the slow mon: these force the holder to go last
        //     in its bracket, explaining the ordering without implying a speed edge.
        if let Some(slow_m) = slow_mon {
            for slow_item in [Item::IronBall, Item::LaggingTail, Item::FullIncense] {
                if !unknown_is_excluded(&slow_m.item, &slow_item) {
                    clause.push(Statement::HasItem {
                        mon_idx: slow_idx,
                        item: slow_item,
                    });
                }
            }
        }

        // (5b) Custap Berry on the fast mon: activates at ≤25% HP and forces the holder
        //      to move first in its priority bracket regardless of speed. Include as an
        //      escape disjunct when the fast mon might be at ≤25% HP (S3 soundness fix).
        if let Some(fast_m) = fast_mon {
            let custap_possible = match &fast_m.hp {
                PokemonHP::Percent(p) => *p <= 25,
                PokemonHP::Number(n) => {
                    let max_hp = fast_m.max_stats[0].max(1) as u32;
                    (*n as u32).saturating_mul(100) / max_hp <= 25
                }
            };
            if custap_possible && !unknown_is_excluded(&fast_m.item, &Item::CustapBerry) {
                clause.push(Statement::HasItem {
                    mon_idx: fast_idx,
                    item: Item::CustapBerry,
                });
            }
        }

        // (6) Weather-conditional speed-doubling abilities on the fast mon.
        //     Only add escapes when the triggering weather was active AS OF this
        //     pairing (mover0's S32 snapshot) — not whatever `state.weather` reads at
        //     Pass 4's call time, which on the second (post-walk) call reflects
        //     end-of-turn weather, not what was active when this pair raced.
        if let Some(fast_m) = fast_mon {
            let pairing_weather = &mover0.weather;
            let is_rain = matches!(pairing_weather, Some(Weather::Rain) | Some(Weather::HeavyRain));
            if is_rain && !unknown_is_excluded(&fast_m.possible_abilities, &Ability::SwiftSwim) {
                clause.push(Statement::HasAbility {
                    mon_idx: fast_idx,
                    ability: Ability::SwiftSwim,
                });
            }
            let is_sun =
                matches!(pairing_weather, Some(Weather::Sun) | Some(Weather::ExtremeSunlight));
            if is_sun && !unknown_is_excluded(&fast_m.possible_abilities, &Ability::Chlorophyll) {
                clause.push(Statement::HasAbility {
                    mon_idx: fast_idx,
                    ability: Ability::Chlorophyll,
                });
            }
            let is_sand = matches!(pairing_weather, Some(Weather::Sandstorm));
            if is_sand && !unknown_is_excluded(&fast_m.possible_abilities, &Ability::SandRush) {
                clause.push(Statement::HasAbility {
                    mon_idx: fast_idx,
                    ability: Ability::SandRush,
                });
            }
            let is_snow = matches!(pairing_weather, Some(Weather::Snow));
            if is_snow && !unknown_is_excluded(&fast_m.possible_abilities, &Ability::SlushRush) {
                clause.push(Statement::HasAbility {
                    mon_idx: fast_idx,
                    ability: Ability::SlushRush,
                });
            }
            // Surge Surfer: ×2 on Electric Terrain (mover0's S32 terrain snapshot).
            if mover0.terrain == Some(Terrain::ElectricTerrain)
                && !unknown_is_excluded(&fast_m.possible_abilities, &Ability::SurgeSurfer)
            {
                clause.push(Statement::HasAbility {
                    mon_idx: fast_idx,
                    ability: Ability::SurgeSurfer,
                });
            }
            // Unburden: ×2 after losing held item.
            if fast_m.item_lost
                && !unknown_is_excluded(&fast_m.possible_abilities, &Ability::Unburden)
            {
                clause.push(Statement::HasAbility {
                    mon_idx: fast_idx,
                    ability: Ability::Unburden,
                });
            }
            // Quick Feet: ×1.5 when statused. Guard whenever the mon is statused;
            // the paralysis factor in `compute_speed_multipliers` already handles the
            // para case numerically, but Quick Feet *overrides* the paralysis penalty,
            // so the predicate may be too strong without this escape when both apply.
            if fast_m.status.is_some()
                && !unknown_is_excluded(&fast_m.possible_abilities, &Ability::QuickFeet)
            {
                clause.push(Statement::HasAbility {
                    mon_idx: fast_idx,
                    ability: Ability::QuickFeet,
                });
            }
        }

        // Emit: unit clause → unconditional bound; multi-entry → disjunction.
        // Guard against duplicate clauses that arise when pass4 is re-run after BCP.
        // Duplicates are logically harmless but cause BCP to re-scan redundant work
        // on every fixpoint iteration.
        if !state.predicates.contains(&clause) {
            state.predicates.push(clause);
        }
    }
}

/// Integer speed multipliers (fast_mult, slow_mult) scaled to a common denominator.
///
/// Encodes: `base_spe(fast) * fast_mult >= base_spe(slow) * slow_mult`.
/// Accounts for boost stages, paralysis (×½), and Tailwind (×2, deterministic).
/// Items (Choice Scarf, Iron Ball) and ability-based multipliers (Swift Swim, etc.)
/// are NOT folded in — they are handled as escape disjuncts in `pass4_speed_from_order`.
///
/// S4: takes the boost/paralysis/Tailwind values EXPLICITLY (as-of the moment the
/// compared pair's ordering was actually observed — see `SpeedFieldsSnapshot` in
/// `pass4_speed_from_order`) rather than reading them live from `state`. An earlier
/// action in the SAME turn (e.g. Thunder Wave paralyzing the second mover, or an
/// Intimidate/boost-move changing a Spe stage) can change these fields mid-turn;
/// reading them live at Pass 4's call time (either before or after the whole turn's
/// events have been walked) can disagree with what actually determined the
/// observed order, baking a wrong numeric factor into a persistent `SpeedComparison`
/// — which `propagate_speed_comparisons` then uses to derive hard Spe bounds, so a
/// wrong factor is a soundness risk, not just imprecision.
fn compute_speed_multipliers(
    fast_boost: i8,
    slow_boost: i8,
    fast_para: bool,
    slow_para: bool,
    fast_tailwind: bool,
    slow_tailwind: bool,
) -> (u32, u32) {
    // Stage multiplier as (numerator, denominator).
    let stage_frac = |stage: i8| -> (u32, u32) {
        let s = stage.clamp(-6, 6);
        if s >= 0 { (2 + s as u32, 2) } else { (2, 2 + (-s) as u32) }
    };

    let (fn_, fd) = stage_frac(fast_boost);
    let (sn_, sd) = stage_frac(slow_boost);
    // Paralysis ×1/2.
    let (fp_n, fp_d): (u32, u32) = if fast_para { (1, 2) } else { (1, 1) };
    let (sp_n, sp_d): (u32, u32) = if slow_para { (1, 2) } else { (1, 1) };
    // Tailwind ×2.
    let (ft_n, ft_d): (u32, u32) = if fast_tailwind { (2, 1) } else { (1, 1) };
    let (st_n, st_d): (u32, u32) = if slow_tailwind { (2, 1) } else { (1, 1) };

    // Combine to a common scale.
    // fast_mult = fn_*fp_n*ft_n * (sd*sp_d*st_d)
    // slow_mult = sn_*sp_n*st_n * (fd*fp_d*ft_d)
    let fast_mult = fn_ * fp_n * ft_n * sd * sp_d * st_d;
    let slow_mult = sn_ * sp_n * st_n * fd * fp_d * ft_d;
    (fast_mult, slow_mult)
}

// ── Pass 5: Back-solve EV / IV / nature from stat bounds ──────────────────────

/// Tighten `min_evs`/`max_evs`/`possible_natures` from current `min_stats`/`max_stats`.
pub fn pass5_back_solve(
    mon: &mut UnknownPokemonState,
    config: &InferenceConfig,
    dex: &HashMap<Species, PokemonData>,
) {
    // S26: a Transformed mon's stats are COPIED from the copy source, not derived
    // from this mon's own species base + EV/IV/nature. Back-solving EVs against the
    // (now copied) species base would be nonsense — and its own EV/IV/nature bounds
    // are preserved in `pre_transform` for the switch-out revert. Skip until it
    // reverts.
    if mon.pre_transform.is_some() {
        return;
    }
    let base: [u16; 6] = match &mon.possible_species {
        Unknown::Known(s) => match dex.get(s) {
            Some(d) => d.base_stats,
            None => return,
        },
        _ => return, // Ambiguous species — skip (sound: we only narrow).
    };
    let level = mon.level;

    let all_natures = ALL_NATURES;
    let candidate_natures: Vec<Nature> = all_natures
        .iter()
        .copied()
        .filter(|n| !unknown_is_excluded(&mon.possible_natures, n))
        .collect();

    if candidate_natures.is_empty() {
        inference_contradiction!("pass5", "no remaining valid natures");
    }

    let ev_candidates: &[u8] = &EV_LATTICE;

    // ── HP (stat_i = 0, no nature modifier) ──────────────────────────────────
    {
        let s_min = mon.min_stats[0];
        let s_max = mon.max_stats[0];
        let iv_lo: u8 = if config.force_max_ivs { 31 } else { mon.min_ivs[0] };
        let iv_hi: u8 = if config.force_max_ivs { 31 } else { mon.max_ivs[0] };
        let mut min_ev: Option<u8> = None;
        let mut max_ev: Option<u8> = None;
        let mut any = false;
        for iv in iv_lo..=iv_hi {
            for &ev in ev_candidates {
                let hp = calc_hp(base[0], iv, ev, level);
                if hp >= s_min && hp <= s_max {
                    any = true;
                    min_ev = Some(min_ev.map_or(ev, |g: u8| g.min(ev)));
                    max_ev = Some(max_ev.map_or(ev, |g: u8| g.max(ev)));
                }
            }
        }
        if !any {
            // S33: `min_stats[0]`/`max_stats[0]` are ONLY ever written by
            // `recompute_stats_for_iv_mode` (full reset against a species) and
            // `recompute_stat_bounds_for_species_change` (re-derived from the mon's
            // CURRENT min_evs[0]/min_ivs[0], hence always reachable by construction —
            // see its doc comment). No damage/percent observation narrows this window
            // (`update_mon_hp` only ever touches the display field `mon.hp`, never
            // `min_stats`), and `Statement::EVIVStatGE/LE` can never target HP (there is
            // no `PokemonStat::Hp` variant — see `stat_to_stats_idx`). So an unreachable
            // window here can ONLY mean `min_stats[0]/max_stats[0]` were computed against
            // a species/context a later resolution (elsewhere in the same fixpoint)
            // superseded WITHOUT going through one of those two reset paths — the same
            // desync class S30 found and fixed for the one call site then known
            // (`HasSpecies` forced mid-BCP after `widen_item_for_illusion`; see
            // `bcp::force_literal`'s `HasSpecies` arm and `IllusionEnded`'s handler,
            // which both perform this exact reset). Rather than crash the whole belief
            // update on any *other*, not-yet-audited call site with the same shape,
            // self-heal the same way: widen back to the current species' theoretical
            // worst/best case and the EV bound back to the full lattice. This is sound
            // — it only WIDENS, so it can never exclude a value that was actually true
            // — at the cost of losing whatever (evidently stale) precision the old
            // window claimed to have.
            let lo = calc_hp(base[0], iv_lo, 0, level);
            let hi = calc_hp(base[0], iv_hi, 252, level);
            mon.min_stats[0] = lo;
            mon.max_stats[0] = hi;
            mon.min_pre_nature_stat[0] = lo;
            mon.max_pre_nature_stat[0] = hi;
            mon.min_evs[0] = 0;
            mon.max_evs[0] = 252;
            return pass5_back_solve(mon, config, dex);
        }
        if let Some(lo) = min_ev
            && lo > mon.min_evs[0] {
                mon.min_evs[0] = lo;
            }
        if let Some(hi) = max_ev
            && hi < mon.max_evs[0] {
                mon.max_evs[0] = hi;
            }
    }

    // ── Non-HP stats (stat_i = 1..=5) ────────────────────────────────────────
    let mut impossible_natures: Vec<bool> = vec![false; candidate_natures.len()];

    for stat_i in 1usize..=5 {
        let s_min = mon.min_stats[stat_i];
        let s_max = mon.max_stats[stat_i];
        let iv_range = if config.force_max_ivs {
            31..=31u8
        } else {
            mon.min_ivs[stat_i]..=mon.max_ivs[stat_i]
        };
        let mut global_min_ev: Option<u8> = None;
        let mut global_max_ev: Option<u8> = None;

        // The pre-nature BSV constraint (from Pass 3 damage inversion) uses the
        // NEUTRAL stat, which is nature-independent — filter the (iv, ev) lattice
        // once here instead of recomputing calc_stat(…, 1.0) for every pair inside
        // the ≤25-nature loop (Pass 5 runs several times per turn).
        let bsv_min = mon.min_pre_nature_stat[stat_i];
        let bsv_max = mon.max_pre_nature_stat[stat_i];
        let feasible_pairs: Vec<(u8, u8)> = iv_range
            .clone()
            .flat_map(|iv| ev_candidates.iter().map(move |&ev| (iv, ev)))
            .filter(|&(iv, ev)| {
                let bsv = calc_stat(base[stat_i], iv, ev, level, 1.0);
                bsv >= bsv_min && bsv <= bsv_max
            })
            .collect();

        for (ni, nature) in candidate_natures.iter().enumerate() {
            if impossible_natures[ni] {
                continue;
            }
            let mods = nature_stat_modifiers(nature);
            let nature_mod = mods[stat_i - 1]; // [atk, def, spa, spd, spe]

            let mut found = false;
            let mut n_min_ev: Option<u8> = None;
            let mut n_max_ev: Option<u8> = None;

            for &(iv, ev) in feasible_pairs.iter() {
                let stat = calc_stat(base[stat_i], iv, ev, level, nature_mod);
                if stat >= s_min && stat <= s_max {
                    found = true;
                    n_min_ev = Some(n_min_ev.map_or(ev, |g: u8| g.min(ev)));
                    n_max_ev = Some(n_max_ev.map_or(ev, |g: u8| g.max(ev)));
                }
            }

            if !found {
                impossible_natures[ni] = true;
            } else {
                if let Some(lo) = n_min_ev {
                    global_min_ev = Some(global_min_ev.map_or(lo, |g: u8| g.min(lo)));
                }
                if let Some(hi) = n_max_ev {
                    global_max_ev = Some(global_max_ev.map_or(hi, |g: u8| g.max(hi)));
                }
            }
        }

        // Soundness assertion: if every candidate nature is infeasible for this
        // stat, we have a contradiction — the observed stat range cannot be
        // produced by any nature.  This fires only if inference itself has
        // over-narrowed (a bug), never for valid opponent data.
        if impossible_natures.iter().all(|&b| b) {
            inference_contradiction!(
                "pass5",
                "every candidate nature is infeasible for stat {stat_i} \
                 (minStat={s_min}, maxStat={s_max}) — inference over-narrowed"
            );
        }

        if let Some(lo) = global_min_ev
            && lo > mon.min_evs[stat_i] {
                mon.min_evs[stat_i] = lo;
            }
        if let Some(hi) = global_max_ev
            && hi < mon.max_evs[stat_i] {
                mon.max_evs[stat_i] = hi;
            }
    }

    // Eliminate natures that were impossible for any stat.
    for (ni, nature) in candidate_natures.iter().enumerate() {
        if impossible_natures[ni] {
            unknown_exclude(&mut mon.possible_natures, nature, "pass5-nature");
        }
    }

    // Panic if every nature is now excluded.
    let remaining = all_natures
        .iter()
        .filter(|n| !unknown_is_excluded(&mon.possible_natures, n))
        .count();
    if remaining == 0 {
        inference_contradiction!("pass5", "no valid nature remains after pass5");
    }

    // ── Global EV total-cap cross-stat tightening ─────────────────────────────
    // Applies only when a cap is configured (default 510 for Pokémon Champions).
    // Sound: only ever tightens max_evs; never raises min_evs.
    // Invariant: Σ_i evs[i] ≤ cap  →  evs[i] ≤ cap − Σ_{j≠i} min_evs[j].
    if let Some(cap) = config.ev_total_cap {
        let min_ev_sum: u16 = (0..6).map(|i| mon.min_evs[i] as u16).sum();
        let ev_lattice = if config.use_stat_points { Some(ev_candidates) } else { None };

        for stat_i in 0..6 {
            let other_min_sum = min_ev_sum - mon.min_evs[stat_i] as u16;
            if other_min_sum >= cap {
                // All other stats already use the full cap — this stat can't have any EVs.
                mon.max_evs[stat_i] = 0;
                continue;
            }
            let budget = cap - other_min_sum; // max EVs allowed in stat_i
            if budget < mon.max_evs[stat_i] as u16 {
                // Round down to the nearest valid lattice value.
                let capped_max = if let Some(lattice) = ev_lattice {
                    lattice
                        .iter()
                        .rev()
                        .find(|&&v| (v as u16) <= budget)
                        .copied()
                        .unwrap_or(0)
                } else {
                    budget.min(252) as u8
                };
                if capped_max < mon.max_evs[stat_i] {
                    mon.max_evs[stat_i] = capped_max;
                }
            }
        }
    }
}

const ALL_NATURES: &[Nature] = &[
    Nature::Hardy,
    Nature::Lonely,
    Nature::Adamant,
    Nature::Naughty,
    Nature::Brave,
    Nature::Bold,
    Nature::Docile,
    Nature::Impish,
    Nature::Lax,
    Nature::Relaxed,
    Nature::Modest,
    Nature::Mild,
    Nature::Bashful,
    Nature::Rash,
    Nature::Quiet,
    Nature::Calm,
    Nature::Gentle,
    Nature::Careful,
    Nature::Quirky,
    Nature::Sassy,
    Nature::Timid,
    Nature::Hasty,
    Nature::Jolly,
    Nature::Naive,
    Nature::Serious,
];

