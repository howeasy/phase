// engine-citation-gate: symbol anchors only
//! P7 v3 (CR 732.2a): capture + drive a MULTI-ACTION mana-engine loop.
//!
//! Real-card acceptance: **Basalt Monolith + Power Artifact** — the canonical 2-card infinite-mana
//! combo. Basalt's `{T}: Add {C}{C}{C}` (an off-stack mana ability, CR 605.3b) then its separate
//! `{3}: Untap this artifact` (on-stack, reduced to `{1}` by Power Artifact, CR 118.9) form ONE
//! loop period of TWO activations whose net progress is `+2 {C}` per cycle while the board returns
//! to equality. This is the class OPTION 2 (multi-action) enables — a single `LoopAction` cannot
//! represent it.
//!
//! Honesty bar: every card is loaded from the real `shared_card_db()` through the real
//! parser+reducer; Power Artifact's cost reduction materializes through the LAYER system
//! (`attach_to` → `flush_layers`); every beat runs through `apply_action` / `GameAction`.

use super::support::shared_card_db;
use engine::analysis::decision_template::IterationCount;
use engine::analysis::loop_check::{ShortcutResponse, WinKind};
use engine::analysis::resource::ResourceAxis;
use engine::database::card_db::CardDatabase;
use engine::game::deck_loading::create_object_from_card_face;
use engine::game::derived_views::{CollapseCertainty, FamilyCollapseState, UnboundedFamily};
use engine::game::effects::attach::attach_to;
use engine::game::mana_abilities::is_mana_ability;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::{add_to_zone, remove_from_zone};
use engine::types::ability::{AbilityKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    CastPaymentMode, GameState, LoopAction, LoopActionContext, LoopDetectionMode, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const BASALT: &str = "Basalt Monolith";
const POWER: &str = "Power Artifact";

/// Place a real card on the battlefield after build, bypassing the unattached-aura attach-choice
/// pause (mirrors `loop_shortcut_activation`). Auras must be placed this way then attached.
fn place_on_battlefield(
    state: &mut GameState,
    player: PlayerId,
    name: &str,
    db: &CardDatabase,
) -> ObjectId {
    let face = db
        .get_face_by_name(name)
        .unwrap_or_else(|| panic!("card '{name}' not found in fixture"));
    let id = create_object_from_card_face(state, face, player);
    remove_from_zone(state, id, Zone::Library, player);
    add_to_zone(state, id, Zone::Battlefield, player);
    state.objects.get_mut(&id).unwrap().zone = Zone::Battlefield;
    id
}

/// The layer-derived mana-ability index on `source` (`{T}: Add {C}{C}{C}`). Read OFF the object.
pub(crate) fn mana_ability_index(state: &GameState, source: ObjectId) -> Option<usize> {
    state
        .objects
        .get(&source)?
        .abilities
        .iter()
        .position(is_mana_ability)
}

/// The layer-derived NON-mana activated ability index on `source` (`{3}: Untap this artifact`).
/// The static "doesn't untap during your untap step" ability is `Static`-kind, so the only
/// non-mana `Activated` ability is the untap.
pub(crate) fn untap_ability_index(state: &GameState, source: ObjectId) -> Option<usize> {
    state
        .objects
        .get(&source)?
        .abilities
        .iter()
        .position(|def| def.kind == AbilityKind::Activated && !is_mana_ability(def))
}

/// Tap an untapped land `player` controls for mana (its mana ability), giving floating mana.
fn tap_untapped_land(runner: &mut GameRunner, player: PlayerId) {
    let land = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            let o = &runner.state().objects[id];
            o.controller == player && !o.tapped && o.card_types.core_types.contains(&CoreType::Land)
        })
        .expect("an untapped land");
    let mana_idx = mana_ability_index(runner.state(), land).expect("land mana ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: land,
            ability_index: mana_idx,
        })
        .expect("tap land for mana");
}

/// Floating colorless mana in `player`'s pool.
fn colorless(state: &GameState, player: PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.mana_pool.count_color(ManaType::Colorless))
        .unwrap_or(0)
}

pub(crate) struct Rig {
    pub(crate) runner: GameRunner,
    pub(crate) basalt: ObjectId,
}

/// Build the 2-player rig: Basalt Monolith on P0's battlefield, optionally with Power Artifact
/// attached (the cost-reduction that makes the untap net-positive). `mode` selects the detector.
pub(crate) fn setup(with_power: bool, mode: LoopDetectionMode, db: &CardDatabase) -> Rig {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let basalt = scenario.add_real_card(P0, BASALT, Zone::Battlefield, db);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    if with_power {
        let power = place_on_battlefield(runner.state_mut(), P0, POWER, db);
        attach_to(runner.state_mut(), power, basalt);
        assert_eq!(
            runner.state().objects[&power].attached_to,
            Some(basalt.into()),
            "Power Artifact must attach to Basalt (attach_to succeeded)"
        );
    }
    Rig { runner, basalt }
}

/// Activate `ability_index` on `source`, then pass priority (both seats) until the stack settles
/// empty at a `Priority` window OR a `LoopShortcut` offer surfaces.
fn activate_and_settle(runner: &mut GameRunner, source: ObjectId, ability_index: usize) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index,
        })
        .expect("activation is legal");
    for _ in 0..60 {
        match &runner.state().waiting_for {
            WaitingFor::LoopShortcut { .. } => break,
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {}
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
}

/// Drive one full loop period: the off-stack mana beat, then the on-stack untap beat, settling
/// each. Returns with the CR 732.2a offer surfaced (if the loop is detected).
pub(crate) fn drive_one_period(rig: &mut Rig, mana_idx: usize, untap_idx: usize) {
    activate_and_settle(&mut rig.runner, rig.basalt, mana_idx);
    activate_and_settle(&mut rig.runner, rig.basalt, untap_idx);
}

/// T1 ⭐ — real Basalt + Power OFFERS a MULTI-ACTION shortcut `[Mana(Colorless)]` / `Advantage` /
/// (Fixed count picker). The whole STEP B/C/D pipeline end-to-end through `apply_action`.
/// Revert-failing: dropping STEP C's sequence drive (driving only `seq[0]`) re-taps the tapped
/// Basalt on the 2nd iteration ⇒ `RecastAbort` ⇒ no offer (the `activation_loop_without_untapper`
/// twin shows the same abort mechanism). The paired negative is T6 (no Power ⇒ net-0 ⇒ no offer).
#[test]
fn mana_engine_basalt_power_offers_mana_advantage_shortcut() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt)
        .expect("Basalt's {T}: Add {C}{C}{C} mana ability");
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt)
        .expect("Basalt's {3}: Untap activated ability");

    drive_one_period(&mut rig, mana_idx, untap_idx);

    // Positive reach-guard: BOTH beats accumulated (armed, non-vacuous) before the offer.
    assert_eq!(
        rig.runner.state().last_loop_action_sequence.len(),
        2,
        "the period is a 2-activation sequence (mana beat + untap beat)"
    );
    match &rig.runner.state().waiting_for {
        WaitingFor::LoopShortcut {
            proposer,
            certificate,
            ..
        } => {
            assert_eq!(*proposer, P0, "the loop's controller proposes the shortcut");
            assert_eq!(
                certificate.unbounded,
                vec![ResourceAxis::Mana(ManaType::Colorless)],
                "the mana-engine certificate names exactly the colorless-mana axis"
            );
            assert_eq!(
                certificate.win_kind,
                WinKind::Advantage,
                "a pure mana engine is an Advantage loop (no lethal/poison/decking axis)"
            );
        }
        other => panic!("expected a CR 732.2a LoopShortcut offer, got {other:?}"),
    }
}

/// T2 — the sequence ACCUMULATES both beats in order. After the mana beat `len==1`; after the
/// untap beat `len==2`, both `Activate`, same controller. Revert-failing: removing the else-arm
/// APPEND branch makes the untap CLEAR (pre-P7 behavior) ⇒ `len` never reaches 2 ⇒ no offer.
#[test]
fn mana_engine_accumulates_both_beats() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).unwrap();

    activate_and_settle(&mut rig.runner, rig.basalt, mana_idx);
    assert_eq!(
        rig.runner.state().last_loop_action_sequence.len(),
        1,
        "the off-stack mana beat SEEDS a 1-step period"
    );
    activate_and_settle(&mut rig.runner, rig.basalt, untap_idx);
    let seq = rig.runner.state().last_loop_action_sequence.clone();
    assert_eq!(seq.len(), 2, "the untap beat APPENDS ⇒ a 2-step period");
    assert!(
        seq.iter()
            .all(|c| matches!(c.action, LoopAction::Activate { .. }) && c.controller == P0),
        "both steps are P0 Activate steps (homogeneous controller)"
    );
}

/// T3 — a PARTIAL period (only the mana beat) does NOT offer. The accumulator arms `[mana]`
/// (non-vacuity), but driving `[mana]` twice re-taps the already-tapped Basalt on the 2nd
/// iteration ⇒ `RecastAbort` ⇒ no offer. The drive+cover IS the period-boundary check. Paired
/// positive = T1 (the full 2-beat period offers).
#[test]
fn mana_engine_partial_period_does_not_offer() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();

    activate_and_settle(&mut rig.runner, rig.basalt, mana_idx);

    assert_eq!(
        rig.runner.state().last_loop_action_sequence.len(),
        1,
        "reach-guard: the mana beat armed a 1-step accumulator (non-vacuous)"
    );
    assert!(
        !matches!(
            rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "a partial [mana] period never covers (Basalt is tapped) ⇒ no offer"
    );
}

/// T6 — Basalt WITHOUT Power Artifact does NOT offer. The untap costs the full `{3}`, exactly what
/// the mana beat produced, so net mana per period is 0 ⇒ `net_progress_for` fails ⇒ no offer. The
/// accumulator still arms both beats (non-vacuity), so rejection is the SIGN-CHECK, not a capture
/// failure. Paired positive = T1 (with Power the untap is `{1}` ⇒ net `+2`).
#[test]
fn mana_engine_without_power_does_not_offer() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(false, LoopDetectionMode::Interactive, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).unwrap();

    drive_one_period(&mut rig, mana_idx, untap_idx);

    assert_eq!(
        rig.runner.state().last_loop_action_sequence.len(),
        2,
        "reach-guard: both beats armed even without Power (rejection is the sign-check, not capture)"
    );
    assert!(
        !matches!(
            rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "without Power the untap costs the full {{3}} ⇒ net-0 mana ⇒ no offer"
    );
}

/// T-HET — capture-level identity protection: a CONTROLLER CHANGE resets the accumulator to a
/// fresh single-controller period, so a heterogeneous (multi-controller) sequence NEVER forms.
/// P0 seeds `[mana(P0)]`; when P1 activates their OWN Basalt's mana beat the accumulator resets to
/// `[mana(P1)]` (not `[mana(P0), mana(P1)]`). Revert-failing: dropping the controller-change reset
/// in `accumulate_loop_action_step` grows a mixed `[P0, P1]` sequence. (The drive's per-step
/// `src.controller != step.controller` re-find in `drive_loop_action_iteration` is the runtime
/// backstop, byte-unchanged from the recast path and covered by the recast tests.)
#[test]
fn mana_engine_controller_change_resets_accumulator() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    // P1 gets their own Basalt so P1 has a mana ability to activate.
    let p1_basalt = place_on_battlefield(rig.runner.state_mut(), P1, BASALT, db);
    let p0_mana = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let p1_mana = mana_ability_index(rig.runner.state(), p1_basalt).unwrap();

    activate_and_settle(&mut rig.runner, rig.basalt, p0_mana);
    let seq = rig.runner.state().last_loop_action_sequence.clone();
    assert_eq!(seq.len(), 1, "P0 seeds a 1-step period");
    assert_eq!(seq[0].controller, P0);

    // Hand priority to P1 and let P1 activate their own mana beat.
    rig.runner.act(GameAction::PassPriority).expect("P0 passes");
    activate_and_settle(&mut rig.runner, p1_basalt, p1_mana);

    let seq = rig.runner.state().last_loop_action_sequence.clone();
    assert_eq!(
        seq.len(),
        1,
        "the controller change RESET the accumulator (no [P0, P1] heterogeneous sequence)"
    );
    assert_eq!(
        seq[0].controller, P1,
        "the reset re-seeded with P1's beat only"
    );
}

/// Create `name` in `player`'s hand after build (mirror of `place_on_battlefield` for Hand).
fn place_in_hand(
    state: &mut GameState,
    player: PlayerId,
    name: &str,
    db: &CardDatabase,
) -> ObjectId {
    let face = db
        .get_face_by_name(name)
        .unwrap_or_else(|| panic!("card '{name}' not found in fixture"));
    let id = create_object_from_card_face(state, face, player);
    remove_from_zone(state, id, Zone::Library, player);
    add_to_zone(state, id, Zone::Hand, player);
    state.objects.get_mut(&id).unwrap().zone = Zone::Hand;
    id
}

/// Give `player` enough Plains to cast Disenchant ({1}{W}) and a Disenchant in hand.
fn arm_disenchant(rig: &mut Rig, player: PlayerId, db: &CardDatabase) -> (ObjectId, CardId) {
    place_on_battlefield(rig.runner.state_mut(), player, "Plains", db);
    place_on_battlefield(rig.runner.state_mut(), player, "Plains", db);
    let disenchant = place_in_hand(rig.runner.state_mut(), player, "Disenchant", db);
    let card_id = rig.runner.state().objects[&disenchant].card_id;
    (disenchant, card_id)
}

/// T-INT-a ⭐ — INTERRUPTIBILITY, UNDEFUSED: P1 HOLDS a real response (Disenchant) but PASSES ⇒
/// the shortcut is GRANTED (offer surfaces). The untap is ON the stack (CR 602.2a; the mana beat is
/// off-stack per CR 605.3b), so P1 has a
/// genuine response window; passing it lets the loop settle and offer. Matched with T-INT-b: P1's
/// pass-vs-respond is the SOLE delta and FLIPS the outcome.
#[test]
fn mana_engine_interruptibility_undefused_opponent_passes_grants() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    let _ = arm_disenchant(&mut rig, P1, db); // P1 could respond, but here PASSES (activate_and_settle auto-passes P1)
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).unwrap();

    drive_one_period(&mut rig, mana_idx, untap_idx);

    assert!(
        matches!(
            &rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { proposer, .. } if *proposer == P0
        ),
        "UNDEFUSED (P1 passes): the loop settles and the shortcut is OFFERED, got {:?}",
        rig.runner.state().waiting_for
    );
    assert!(
        rig.runner.state().objects.contains_key(&rig.basalt)
            && rig.runner.state().objects[&rig.basalt].zone == Zone::Battlefield,
        "Basalt survives (P1 did not respond)"
    );
}

/// T-INT-b ⭐ — INTERRUPTIBILITY, DEFUSED: P1 RESPONDS to the untap (on the stack, CR 602.2a; the
/// mana beat is off-stack per CR 605.3b) by
/// casting Disenchant on Basalt. Basalt is destroyed, the untap resolves against nothing, and at
/// the settle the drive's per-step `ObjectId` re-find fails (Basalt gone) ⇒ NO offer beyond the
/// stack. The ONLY delta vs T-INT-a is P1's respond-vs-pass, and the outcome FLIPS (offer → no
/// offer). Non-vacuity: T-INT-a proves the same board OFFERS when P1 passes.
#[test]
fn mana_engine_interruptibility_defused_opponent_responds_no_grant() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    let (disenchant, dis_card) = arm_disenchant(&mut rig, P1, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).unwrap();

    // P0: mana beat (off-stack), settles to P0 priority.
    activate_and_settle(&mut rig.runner, rig.basalt, mana_idx);
    // P0: untap beat (ON the stack).
    rig.runner
        .act(GameAction::ActivateAbility {
            source_id: rig.basalt,
            ability_index: untap_idx,
        })
        .expect("untap activation is legal");
    // P0 passes ⇒ P1 gets priority with the untap on the stack (the real response window).
    rig.runner.act(GameAction::PassPriority).expect("P0 passes");
    // P1 RESPONDS: Disenchant destroys Basalt in response to the untap. The reducer surfaces a
    // `TargetSelection` prompt (the action's `targets` field is not consumed by the reducer), which
    // we answer with Basalt.
    rig.runner
        .act(GameAction::CastSpell {
            object_id: disenchant,
            card_id: dis_card,
            targets: vec![rig.basalt],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 may cast Disenchant in response (instant speed)");
    // Settle everything (Disenchant targets Basalt, resolves, destroys it; then the untap resolves
    // against a destroyed Basalt).
    for _ in 0..60 {
        match rig.runner.state().waiting_for.clone() {
            WaitingFor::LoopShortcut { .. } => break,
            WaitingFor::TargetSelection { .. } => {
                rig.runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(rig.basalt)],
                    })
                    .expect("Disenchant targets Basalt (a legal artifact)");
            }
            WaitingFor::Priority { .. } if rig.runner.state().stack.is_empty() => break,
            _ => {
                if rig.runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }

    assert!(
        rig.runner.state().objects.get(&rig.basalt).map(|o| o.zone) != Some(Zone::Battlefield),
        "reach-guard: P1's Disenchant destroyed Basalt (the response landed)"
    );
    assert!(
        !matches!(
            rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "DEFUSED (P1 responds): Basalt is gone ⇒ the drive's re-find aborts ⇒ NO grant, got {:?}",
        rig.runner.state().waiting_for
    );
}

/// P2 (updated 2026-07-18, user directive): Accept on an unbounded MANA engine MARKS the
/// certificate's `Mana(_)` axes via `mark_unbounded_loop` (reusing the infinite-mana machinery)
/// rather than driving N finite periods. The `refill_infinite_mana` pipeline top-up (engine.rs,
/// after every action) then holds the flagged player's pool at `INFINITE_MANA_PER_TYPE`, so the
/// grant is genuine infinite mana — treated as actually infinite within the phase (CR 500.4
/// empties + finite-resolves it at the boundary) and INDEPENDENT of the declared count. Returns
/// `(colorless_delta, flagged_infinite)`.
fn accept_mana_engine(db: &CardDatabase, n: u32) -> (i64, bool) {
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).unwrap();
    drive_one_period(&mut rig, mana_idx, untap_idx);
    assert!(
        matches!(
            rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "precondition: the offer must fire before acceptance"
    );
    let at_offer = colorless(rig.runner.state(), P0) as i64;
    rig.runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(n),
            template: None,
        })
        .expect("declare shortcut");
    rig.runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("opponent accepts");
    let flagged = rig
        .runner
        .state()
        .unbounded_resources
        .get(&P0)
        .is_some_and(|axes| axes.iter().any(|a| matches!(a, ResourceAxis::Mana(_))));
    (colorless(rig.runner.state(), P0) as i64 - at_offer, flagged)
}

/// The ∞-mark is count-INDEPENDENT and yields genuine infinite mana (pool held at
/// `INFINITE_MANA_PER_TYPE`), not the old finite `+2·n`. DISCRIMINATING (revert-probe): without
/// the `mark_unbounded_loop` call, `flagged` is false and the pool is never topped ⇒ both the
/// flag and the ≥90 jump flip to fail.
#[test]
fn mana_engine_accept_marks_infinite_mana_independent_of_count() {
    let Some(db) = shared_card_db() else { return };
    let (delta1, flagged1) = accept_mana_engine(db, 1);
    let (delta5, flagged5) = accept_mana_engine(db, 5);
    assert!(
        flagged1 && flagged5,
        "accept must flag P0 with a Mana axis (∞ mana), not drive N finite periods"
    );
    assert_eq!(
        delta1, delta5,
        "the ∞ mark is count-independent (contrast the old drive-N: +2 vs +10)"
    );
    // At offer the pool held +2 from the one detection period; refill tops all colors to
    // INFINITE_MANA_PER_TYPE (100) ⇒ a large count-independent jump (≥90).
    assert!(
        delta1 >= 90,
        "the pool must be topped to the infinite-mana constant, got {delta1}"
    );
}

/// DESIGN STEP 4 (∞-pile) — MANA-ENGINE PAIRED NEGATIVE: accepting a MANA loop marks the
/// `Mana(_)` axis (reach-guard proving the accept genuinely materialized) but writes NO
/// `unbounded_loop_pile` — a mana engine reproduces no fodder token, so
/// `current_period_fodder` returns `None` and no pile is snapshotted. This proves the
/// fodder gate in `materialize_object_growth_shortcut` discriminates object-growth from mana.
///
/// DISCRIMINATING: the Mana-axis assertion is the positive reach-guard (the accept ran and
/// marked ∞); the empty-pile assertion is the fodder-gate discriminator. Its object-growth
/// counterpart (`combo_infinite_pile.rs`) writes a NON-empty pile from the same accept seam.
#[test]
fn mana_engine_accept_writes_no_pile_but_marks_mana() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).unwrap();
    drive_one_period(&mut rig, mana_idx, untap_idx);
    assert!(
        matches!(
            rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "precondition: the mana-engine offer must fire before acceptance"
    );
    rig.runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1),
            template: None,
        })
        .expect("declare shortcut");
    rig.runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("opponent accepts");

    // Positive reach-guard: the accept materialized and marked the Mana axis.
    assert!(
        rig.runner
            .state()
            .unbounded_resources
            .get(&P0)
            .is_some_and(|axes| axes.iter().any(|a| matches!(a, ResourceAxis::Mana(_)))),
        "the mana-engine accept must mark a Mana(_) axis (reach-guard)"
    );
    // Fodder-gate discriminator: a mana engine reproduces no token ⇒ no ∞ pile.
    assert!(
        rig.runner.state().unbounded_loop_pile.is_empty(),
        "a mana engine has no fodder class ⇒ no unbounded_loop_pile is written"
    );
}

/// T5-analog — `Off` byte-identity (#4603). Under `LoopDetectionMode::Off` the mana engine NEVER
/// arms the sequence (the `samples()` gate) and NEVER offers, while the game plays normally (Basalt
/// untaps, mana is in the pool). Revert-failing: dropping the `samples()` gate on the mana-arm /
/// else-arm capture writes the sequence under `Off`.
#[test]
fn mana_engine_off_mode_is_byte_identical() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Off, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).unwrap();

    drive_one_period(&mut rig, mana_idx, untap_idx);

    assert!(
        rig.runner.state().last_loop_action_sequence.is_empty(),
        "Off (#4603): the mana engine must NOT arm the sequence"
    );
    assert!(
        !matches!(
            rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "Off never samples ⇒ never offers"
    );
    assert!(
        !rig.runner.state().objects[&rig.basalt].tapped,
        "Off plays normally: the untap resolved and Basalt is untapped"
    );
    assert!(
        colorless(rig.runner.state(), P0) >= 2,
        "Off plays normally: the mana beat produced mana (net +2 after the untap)"
    );
}

/// FIX-3 (CR 732.2a, CONDITIONAL load migration): `last_loop_action_sequence` deserializes NORMALLY
/// (its `pins` round-trip — B2 restored), but the PRODUCTION restore hook
/// `PersistedGameState::into_game_state` → `GameState::migrate_transient_loop_sequence` DROPS it on
/// load UNLESS the save sits in an object-growth shortcut proposal/response window
/// (`WaitingFor::LoopShortcut` / `RespondToShortcut`), whose pending accept→materialize resolution
/// re-derives the ∞ pile from the sequence. This REPLACES the Design-A blanket `#[serde(skip)]`
/// (always-drop) contract, which regressed the predecessor `combo_infinite_pile` offer-saves by
/// starving accept→materialize of the pile.
///
/// DISCRIMINATING — the ONLY guard on the load migration + the B2 pins round-trip (the field is
/// EXCLUDED from `impl PartialEq for GameState`). Parts (a) and (b) round-trip the SAME populated,
/// PINNED sequence through the real production hook and differ ONLY in `waiting_for`, so the
/// outcome FLIPS: a hook that ignored `waiting_for` (Design A, always-drop) fails (b); a hook that
/// never dropped fails (a). Part (b) additionally asserts the pin survived (Design A dropped pins).
#[test]
fn loop_action_sequence_conditional_load_migration() {
    use engine::analysis::decision_template::{
        DecisionSlot, PinnedDecision, ShortcutDecisionSchema,
    };
    use engine::analysis::loop_check::LoopCertificate;
    use engine::analysis::resource::BoardDelta;
    use engine::types::game_state::{PersistedGameState, YieldTarget};
    use engine::types::mana::ManaColor;

    let mana_color_pin = || PinnedDecision::ManaColor {
        slot: DecisionSlot {
            source: YieldTarget::ThisObject {
                source_id: ObjectId(7),
                incarnation: None,
                trigger_description: None,
            },
            index: 1,
        },
        color: ManaColor::Blue,
    };
    let pinned_step = || LoopActionContext {
        stack_depth: 0,
        card_id: CardId(4242),
        controller: P0,
        action: LoopAction::Activate {
            source_id: ObjectId(7),
            ability_index: 1,
        },
        convoke: None,
        pins: vec![mana_color_pin()],
    };

    // (a) captured at empty-stack `Priority` (NOT a shortcut window) → the production hook DROPS the
    //     sequence. It deserializes NON-EMPTY first, proving the drop is the migration hook, not the
    //     `#[serde(skip)]` derive (which Design A used and which regressed the predecessor tests).
    let mut at_priority = GameState::new_two_player(1);
    at_priority.waiting_for = WaitingFor::Priority { player: P0 };
    at_priority.last_loop_action_sequence = vec![pinned_step(), pinned_step()];
    let raw = serde_json::to_string(&at_priority).expect("serialize");
    assert!(
        raw.contains("last_loop_action_sequence"),
        "a populated sequence IS serialized (skip_serializing_if only skips the EMPTY case)"
    );
    let deserialized: GameState = serde_json::from_str(&raw).expect("deserialize");
    assert_eq!(
        deserialized.last_loop_action_sequence.len(),
        2,
        "the sequence deserializes NORMALLY (len 2) — the drop is the load hook, not the derive"
    );
    let restored = PersistedGameState::Raw(Box::new(at_priority))
        .into_game_state()
        .expect("persisted test snapshot satisfies the checked restore contract");
    assert!(
        restored.last_loop_action_sequence.is_empty(),
        "FIX-3: a Priority-captured save DROPS the transient sequence on load"
    );

    // (b) captured at a `LoopShortcut` offer window → the production hook KEEPS the sequence, and the
    //     recorded pin round-trips (B2). SAME sequence as (a); ONLY `waiting_for` differs ⇒ the
    //     keep/drop outcome flips, isolating the discriminator to `waiting_for`.
    let mut at_offer = GameState::new_two_player(1);
    at_offer.waiting_for = WaitingFor::LoopShortcut {
        proposer: P0,
        predicted_winner: None,
        certificate: LoopCertificate {
            unbounded: vec![ResourceAxis::TokensCreated],
            win_kind: WinKind::Advantage,
            mandatory: false,
            residual_board_delta: BoardDelta::default(),
            per_cycle: None,
        },
        schema: ShortcutDecisionSchema::default(),
        declaration: None,
    };
    at_offer.last_loop_action_sequence = vec![pinned_step()];
    let json = serde_json::to_string(&at_offer).expect("serialize offer save");
    let reloaded: GameState = serde_json::from_str(&json).expect("deserialize offer save");
    let restored_offer = PersistedGameState::Raw(Box::new(reloaded))
        .into_game_state()
        .expect("persisted test snapshot satisfies the checked restore contract");
    assert_eq!(
        restored_offer.last_loop_action_sequence.len(),
        1,
        "FIX-3: a LoopShortcut-captured offer-save KEEPS the sequence on load (accept→materialize needs it)"
    );
    assert_eq!(
        restored_offer.last_loop_action_sequence[0].pins,
        vec![mana_color_pin()],
        "B2: the recorded pins round-trip for a kept offer-save (Design A's #[serde(skip)] dropped them)"
    );

    // (c) an empty sequence is skipped on the wire and a missing field defaults to empty (UNCHANGED).
    let empty = GameState::new_two_player(1);
    let json = serde_json::to_string(&empty).expect("serialize empty");
    assert!(
        !json.contains("last_loop_action_sequence"),
        "an empty sequence is skipped on the wire (skip_serializing_if)"
    );
    let back: GameState = serde_json::from_str(&json).expect("deserialize missing field");
    assert!(
        back.last_loop_action_sequence.is_empty(),
        "a missing field defaults to an empty Vec"
    );

    // (d) a step's `stack_depth` round-trips, a zero depth is omitted on the wire, and a sequence
    //     saved before the field existed loads with every depth 0. The sequence is a real
    //     response-timed lap (tap@0, untap@0, tap@1).
    let mut rig = lap_rig(LapBoard {
        synthetic: &[COPIER],
        colorless_seed: 2,
        ..LapBoard::BARE
    });
    human_lap(&mut rig, &[], true);
    let seq = rig.runner.state().last_loop_action_sequence.clone();
    assert_eq!(
        seq.iter().map(|s| s.stack_depth).collect::<Vec<_>>(),
        vec![0, 0, 1],
        "reach guard: the lap recorded one response step"
    );
    let json = serde_json::to_string(&seq).expect("serialize the sequence");
    assert_eq!(
        json.matches("\"stack_depth\"").count(),
        1,
        "only the depth-1 step carries the key"
    );
    let back: Vec<LoopActionContext> = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, seq, "the depths round-trip");
    let mut old: serde_json::Value = serde_json::from_str(&json).expect("a JSON array");
    for step in old.as_array_mut().expect("an array") {
        step.as_object_mut()
            .expect("a step object")
            .remove("stack_depth");
    }
    let old: Vec<LoopActionContext> = serde_json::from_value(old).expect("a key-less sequence");
    assert_eq!(
        old.iter().map(|s| s.stack_depth).collect::<Vec<_>>(),
        vec![0, 0, 0],
        "a key-less step loads at depth 0"
    );
}

/// ⭐ COND A — the crux measurement (team-lead PATH-2 (iii)): does a per-cycle action that depletes
/// a FINITE OPPONENT resource become ILLEGAL / error at exhaustion (which would make a
/// break-on-err flip demonstrable, PATH-1), or does it NO-OP (which makes the loop genuinely
/// infinite-advantage, not finite-fuel, ⇒ PATH-2)?
///
/// The ONLY opponent-resource-depleting action that can be DRIVEN (offered) is a NON-targeted one
/// (a targeted one raises a `TargetSelection` the drive answers with `RecastAbort` — it is never
/// offered, so it can't reach materialize). Pyrohemia's `{R}: deals 1 damage to each creature and
/// each player` is exactly that: a repeatable, non-targeted activated ability that depletes a
/// finite opponent resource (the 2/2's toughness/existence). We drive it PAST exhaustion and
/// MEASURE the reducer result.
///
/// RESULT (measured): the post-exhaustion activation is LEGAL and fully RESOLVES (it no-ops on the
/// absent creatures, still hits players) — it does NOT error and does NOT become illegal. So no
/// offered loop's drive aborts at an opponent's resource boundary ⇒ there is NO finite opp-fuel
/// loop for the `if drive.is_err() break` to self-limit ⇒ PATH-2: the break is a DEFENSIVE guard
/// over a provably-empty class (cost-fuel is CR 601.2f/602.2b/118.3 CASE 0; controller-fuel is
/// firewall-vetoed pre-offer, `sign_check_object_counter_decrease_rejects`). Revert-failing for the
/// (iii)(a) claim: if the reducer ever made a non-targeted depletion ILLEGAL at exhaustion, the
/// `res.is_ok()` / `is_creature` reach-guard pair would flip.
#[test]
fn cond_a_nontargeted_opponent_depletion_noops_at_exhaustion_not_abort() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pyro = scenario.add_real_card(P0, "Pyrohemia", Zone::Battlefield, db);
    for _ in 0..3 {
        scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db);
    }
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let mut runner = scenario.build();
    // Off keeps the offer machinery out of the way — this measures the REDUCER's exhaustion
    // behavior (mode-independent), not an offer.
    runner.state_mut().loop_detection = LoopDetectionMode::Off;

    // Pyrohemia's only non-mana Activated ability is the `{R}: damage-each` ability.
    let dmg_idx =
        untap_ability_index(runner.state(), pyro).expect("Pyrohemia's {R}: damage-each ability");

    // Two activations (2 damage) kill the 2/2 Bears — the finite opponent resource is exhausted.
    for _ in 0..2 {
        tap_untapped_land(&mut runner, P0);
        activate_and_settle(&mut runner, pyro, dmg_idx);
    }
    assert!(
        runner.state().objects.get(&bears).map(|o| o.zone) != Some(Zone::Battlefield),
        "reach-guard: two non-targeted pings killed the 2/2 (opponent resource depleted)"
    );
    let creatures_left = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner.state().objects[id]
                .card_types
                .core_types
                .contains(&CoreType::Creature)
        })
        .count();
    assert_eq!(
        creatures_left, 0,
        "reach-guard: no creatures remain (fully exhausted)"
    );

    // THE MEASUREMENT: activate the SAME non-targeted depletion action AGAIN, resource exhausted.
    tap_untapped_land(&mut runner, P0);
    let res = runner.act(GameAction::ActivateAbility {
        source_id: pyro,
        ability_index: dmg_idx,
    });
    assert!(
        res.is_ok(),
        "(iii)(a): a non-targeted opponent-depletion activation is LEGAL at exhaustion (no target \
         requirement) — it does NOT become illegal"
    );
    // Drive it to full resolution: it must NOT abort/error (it no-ops on the absent creatures).
    for _ in 0..20 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty()
        {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "(iii)(a): the depletion action fully RESOLVED at exhaustion (no-op, no error, no abort) ⇒ \
         no finite opp-fuel loop exists ⇒ the break-on-err is a defensive guard (PATH-2)"
    );
}

/// R6a-2, RECLASSIFIED under option (B): a CHANNEL-LIVENESS row, no longer a discriminator.
/// A mana engine registers NO deferred materialization — `current_period_fodder` finds no
/// fodder, `current_period_counter_growth` / `current_period_life_growth` are empty — so
/// nothing will ever collapse its `Mana(_)` axis at the CR 500.5 boundary. It is genuinely
/// unbounded within the phase (`refill_infinite_mana` holds the pool at
/// `INFINITE_MANA_PER_TYPE`) and MUST keep rendering its `∞` row on the wire. That claim is
/// true, user-visible and revert-detectable (RP-6 below) — it is simply no longer the thing
/// that distinguishes candidate implementations, because option (B) projects EVERY ∞ row.
///
/// WHY IT NO LONGER DISCRIMINATES: reach-guard (2) below asserts `pending_unbounded_
/// materialization` is EMPTY on this rig, so no schedule-keyed hide filter — stash-keyed or
/// count-keyed (`pending_materialization_count` is empty here too, asserted by the sibling
/// `mana_engine_accept_records_no_collapse_bound`) — could fire against this row anyway. The
/// schedule-independence discrimination therefore lives on rigs where a schedule IS present:
/// `loop_shortcut::unregistered_axis_still_renders_its_infinity_badge` and
/// `scheduled_drive_still_renders_the_already_spendable_mana_badge` below, whose ONE stash names
/// both a `Mana(_)` and a deferred `Life(P0)`.
///
/// REVERT-PROBE (RP-6, RUN): append `views.unbounded_resources.clear();` at the END of
/// `derive_views` (re-kill the row channel unconditionally) ⇒ this row FAILS while the ∞ PILE
/// assertion in `loop_shortcut::scheduled_collapse_still_renders_the_unbounded_badge`
/// stays green — a different channel.
///
/// The `shared_card_db()` guard below is DORMANT in a normal checkout: `integration_cards.json.gz`
/// is tracked, so it only fires in a checkout without the card-data pipeline.
#[test]
fn mana_engine_accept_still_renders_its_infinity_badge() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).unwrap();
    drive_one_period(&mut rig, mana_idx, untap_idx);
    assert!(
        matches!(
            rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "precondition: the mana-engine offer must fire before acceptance"
    );
    rig.runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1),
            template: None,
        })
        .expect("declare shortcut");
    rig.runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("opponent accepts");

    let state = rig.runner.state();
    // (1) reach-guard: the accept ran and marked a Mana axis in the store.
    assert!(
        state
            .unbounded_resources
            .get(&P0)
            .is_some_and(|axes| axes.iter().any(|a| matches!(a, ResourceAxis::Mana(_)))),
        "reach-guard: the mana-engine accept marks a Mana(_) ∞ axis"
    );
    // (2) reach-guard: it registered NOTHING — this is the unscheduled-axis shape.
    assert!(
        state.pending_unbounded_materialization.is_empty(),
        "reach-guard: a mana engine registers no deferred materialization, got {:?}",
        state.pending_unbounded_materialization
    );

    // (3) DISCRIMINATOR — on the WIRE, the Mana row still renders for every viewer.
    for viewer in [None, Some(P0), Some(P1)] {
        let rows = engine::game::derived_views::derive_views(state, viewer).unbounded_resources;
        assert!(
            rows.iter().any(|r| matches!(r.axis, ResourceAxis::Mana(_))),
            "FAIL-CLOSED: nothing is scheduled to collapse the mana axis, so its ∞ row must \
             still render (viewer {viewer:?}), got {rows:?}"
        );
    }
}

/// R6a FIX-ROUND-2 (CR 732.2c). MEASURED REGRESSION in the first cut of the collapse bound:
/// `materialize_fixed_shortcut` wrote `pending_materialization_count` UNCONDITIONALLY, before
/// either route ran. A mana engine reaches that function and registers NO deferred
/// materialization (proved by
/// [`mana_engine_accept_still_renders_its_infinity_badge`]'s reach-guard (2)) — so a
/// `Fixed(1)` mana accept left a bound with NOTHING to bound.
///
/// That stray bound is UNCLEARABLE and PERSISTENT: all three clears
/// (`take_pending_materialization`, `clear_collapsed_materializations`,
/// `clear_unbounded_loop`) are keyed on the stash, `clear_unbounded_mana_loop` deliberately
/// does not touch it, and the field is `#[serde(default)]`. It therefore survives the phase,
/// the game and a save/load — and the NEXT accept that really does register a stash gets
/// `min(1, N)` = 1. A table that unanimously agreed to `Fixed(500)` object growth would be
/// offered `max: 1` at the CR 500.5 boundary and have `SubmitPayAmount { 500 }` REJECTED with
/// `"[0, 1]"`. BASE offered `MAX_SHORTCUT_CYCLES` and honored the agreed 500, so this was a
/// regression against BASE, not merely an incomplete fix.
///
/// REVERT-PROBE (RUN, MEASURED): hoist the write back out of the stash-gate in
/// `materialize_fixed_shortcut` ⇒ assertion (3) FAILS with
/// `pending_materialization_count = {PlayerId(0): 1}`. (3) short-circuits the run, so (5) is
/// not reached in the same execution; a SECOND probe run with (3) temporarily downgraded to an
/// `eprintln!` reached it and observed (5) FAIL with `left: Some(1) right: Some(1000)`.
///
/// HONEST SCOPE. Two real accepts — a stash-less one followed by a stash-bearing one — need a
/// board carrying BOTH a mana engine and an object-growth loop; that is not this rig and is
/// not reachable here without building a second combo. So the consequence at (4)/(5) is
/// pinned on the REAL `turns.rs` `max` read instead: the stash is grafted through the same
/// single-authority writer the accept path itself calls
/// (`GameState::register_pending_materialization`), and the CR 500.5 boundary is then reached
/// by passing priority through the real `apply()` reducer.
#[test]
fn mana_engine_accept_records_no_collapse_bound() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).unwrap();
    drive_one_period(&mut rig, mana_idx, untap_idx);
    assert!(
        matches!(
            rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "precondition: the mana-engine offer must fire before acceptance"
    );
    rig.runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1),
            template: None,
        })
        .expect("declare shortcut");
    rig.runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("opponent accepts");

    // (1) REACH-GUARD: the accept really ran `materialize_fixed_shortcut` — it marked the
    // Mana axis, which only the materialize path does. Without this the emptiness at (3)
    // would be the vacuous "nothing happened" pass.
    assert!(
        rig.runner
            .state()
            .unbounded_resources
            .get(&P0)
            .is_some_and(|axes| axes.iter().any(|a| matches!(a, ResourceAxis::Mana(_)))),
        "reach-guard: the mana-engine accept reaches materialize_fixed_shortcut and marks a \
         Mana(_) ∞ axis"
    );
    // (2) REACH-GUARD: and it registered NOTHING — there is no stash for a bound to bound.
    assert!(
        rig.runner
            .state()
            .pending_unbounded_materialization
            .is_empty(),
        "reach-guard: a mana engine registers no deferred materialization, got {:?}",
        rig.runner.state().pending_unbounded_materialization
    );

    // (3) DISCRIMINATOR: no bound is recorded either. Asserted on the SAME state the two
    // reach-guards above measured as post-accept and stash-less.
    assert!(
        rig.runner.state().pending_materialization_count.is_empty(),
        "CR 732.2c: a stash-less accept must record NO collapse bound (it would be \
         unclearable and would cap the next accept), got {:?}",
        rig.runner.state().pending_materialization_count
    );

    // (4) CONSEQUENCE, on the real `turns.rs` read. Graft a stash through the production
    // single-authority writer (as if a later object-growth accept had registered one) and
    // reach the CR 500.5 boundary through real priority passes.
    rig.runner.state_mut().register_pending_materialization(
        P0,
        engine::types::game_state::PersistentAxisMaterialization::Life {
            player: P0,
            per_cycle_delta: 1,
        },
    );
    let mut prompt_max = None;
    for _ in 0..64 {
        if let WaitingFor::PayAmountChoice {
            resource: engine::types::game_state::PayableResource::LoopCollapse { .. },
            max,
            ..
        } = rig.runner.state().waiting_for
        {
            prompt_max = Some(max);
            break;
        }
        if rig.runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }

    // (5) The grafted stash carries NO accepted bound, so the prompt falls back to the
    // engine-wide safety bound. Under the unconditional write the stray `Fixed(1)` mana
    // bound is still sitting in the map and caps this prompt at 1.
    // 1_000 is `game::engine::MAX_SHORTCUT_CYCLES`, spelled literally because the const is
    // `pub(crate)` and this is an integration test.
    assert_eq!(
        prompt_max,
        Some(1_000),
        "CR 732.2c: a stash-less mana accept must not bound a LATER stash's collapse prompt"
    );
}

/// CR 500.5, the MULTI-AXIS row: the
/// `PersistentAxisMaterialization::DriveSequence` arm of `scheduled_collapse_axes` returns
/// WHATEVER `collapsed_axes` the stash carries. Production stores only the `DeferredAccrual`
/// subset (`engine::analysis::resource::ResourceAxis::unbounded_mark_kind`); this row's stash is
/// grafted BROADER on purpose — see (ii) below — so ONE stash here names TWO
/// axes — an already-materialized `Mana(Colorless)` and a deferred `Life(P0)`. Both keep their ∞
/// row while the collapse is merely scheduled, and they get there for DIFFERENT reasons, which is
/// what makes this the strongest rig in the file for the projection's schedule-independence.
///
/// The `Life(P0)` axis is DEFERRED: no life has been gained, and none will be until the CR 500.5
/// boundary applies the growth. The growth is in flight along CR 732.2c's advance to the
/// proposal's ending point (`types::game_state`'s `scheduled_collapse_axes` doc). For the DISPLAY
/// what matters is only
/// that the mark and its enablers are still live through the window, so the ∞ renders current
/// engine state rather than a stale mark.
///
/// The `Mana(Colorless)` axis is ALREADY MATERIALIZED at accept:
/// `mana_payment::refill_infinite_mana` re-tops the flagged pool to `INFINITE_MANA_PER_TYPE` off
/// `unbounded_resources` (the STORE, which the projection deliberately never filters) after every
/// action, so throughout the accept→boundary window the player can really spend an unbounded
/// pool. CR 500.5 is what ends that badge: the step/phase end drains the pool and
/// `turns::drain_pending_phase_transition_progress` clears the axis (covered by
/// `combo_infinite_pile`'s E4 mana axis-clear row, not re-proved here) — NOT a materialization.
///
/// HONEST SCOPE. Everything except one write is real: real cards through the real parser, a real
/// two-beat Basalt+Power period, a real `DeclareShortcut`/`RespondToShortcut` accept that marks
/// `Mana(Colorless)` and holds the pool at the cap. What is NOT reachable on this rig — and the
/// no production board reaches either — is a single loop spanning
/// BOTH a `Mana(_)` axis and an OBSERVED counter/life axis, which is what routes an accept into
/// the `DriveSequence` arm (`game::engine::materialize_object_growth_shortcut`). So the stash is
/// grafted through the same single-authority writers the accept path itself calls
/// (`GameState::mark_unbounded_loop` for the second axis, `register_pending_materialization` for
/// the item), with `collapsed_axes` set to exactly the store's mark set — **(ii)** a DELIBERATE
/// SUPERSET of what production writes, not a mirror of it. Production filters `Mana(_)` out of
/// `collapsed_axes` (`ResourceAxis::unbounded_mark_kind`), so a real accept could never name the
/// mana axis here. The graft names it anyway, because this row is about the PROJECTION — "if a
/// mana axis WERE scheduled, does the badge still render the spendable pool?" — and a graft
/// narrowed to production's own output would make the mana half of the row UNREACHABLE rather than
/// merely hostile. Same graft technique as
/// `combo_infinite_pile::real_4p_observed_drive_sequence_replays_captured_period_n_times`.
///
/// REVERT-PROBE: restore `if collapse_scheduled(controller, &axis) { continue; }` in
/// `derive_views`' resource-row loop ⇒ (6) FAILS — `Life(P0)` is in the `DriveSequence`'s
/// `collapsed_axes`, so the restored guard hides its row. (5) is the paired control that keeps
/// the probe honest: BASE also carried an `axes.retain(|a| !matches!(a, ResourceAxis::Mana(_)))`
/// on the hide-set, so the mana row survived that guard and (5) stayed green — a blanket "hide
/// every scheduled axis" and a blanket "hide nothing" are distinguished by this pair.
#[test]
fn scheduled_drive_still_renders_the_already_spendable_mana_badge() {
    use engine::types::game_state::PersistentAxisMaterialization;

    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).unwrap();
    drive_one_period(&mut rig, mana_idx, untap_idx);
    assert!(
        matches!(
            rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "precondition: the mana-engine offer must fire before acceptance"
    );
    // The real captured two-beat period, read AT THE OFFER — `materialize_fixed_shortcut`
    // clears `last_loop_action_sequence` on its way out, and production reads it at the same
    // pre-clear point (`game::engine`'s capture-before-clear).
    let sequence = rig.runner.state().last_loop_action_sequence.clone();
    assert!(
        sequence.len() == 2,
        "reach-guard: the offer carries the real two-beat Basalt+Power period the DriveSequence \
         would replay, got {} beats",
        sequence.len()
    );
    rig.runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1),
            template: None,
        })
        .expect("declare shortcut");
    rig.runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("opponent accepts");

    // (1) REACH-GUARD: the real accept marked the Mana axis in the STORE. Capture the exact
    // axes — the graft below reuses them as the deliberate `collapsed_axes` superset.
    let mana_axes: Vec<ResourceAxis> = rig
        .runner
        .state()
        .unbounded_resources
        .get(&P0)
        .expect("reach-guard: the mana-engine accept marks P0's ∞ axes")
        .iter()
        .copied()
        .filter(|a| matches!(a, ResourceAxis::Mana(_)))
        .collect();
    assert!(
        mana_axes.contains(&ResourceAxis::Mana(ManaType::Colorless)),
        "reach-guard: Basalt+Power nets colorless, so the accept marks Mana(Colorless), got \
         {mana_axes:?}"
    );

    // (2) REACH-GUARD: that axis is ALREADY SPENDABLE — the pipeline refill holds the pool at
    // the infinite-mana cap right now. This is what makes hiding the badge a lie rather than a
    // harmless early cleanup. (`INFINITE_MANA_PER_TYPE` is `pub(crate)`; 100 spelled literally,
    // matching `real_4p_basalt_power_artifact_refills_colorless_only`.)
    let pool = colorless(rig.runner.state(), P0);
    assert!(
        pool >= 90,
        "reach-guard: refill_infinite_mana holds P0's colorless pool at the cap (~100) during \
         the accept→CR-500.5 window, got {pool}"
    );

    // (3) GRAFT (see HONEST SCOPE): a second, genuinely DEFERRED axis plus the one
    // `DriveSequence` an observed-growth accept would register over the real captured period.
    // Both writes go through the production single-authority writers.
    rig.runner
        .state_mut()
        .mark_unbounded_loop(P0, &[ResourceAxis::Life(P0)]);
    let collapsed_axes: Vec<ResourceAxis> = rig
        .runner
        .state()
        .unbounded_resources
        .get(&P0)
        .expect("both axes marked")
        .iter()
        .copied()
        .collect();
    rig.runner.state_mut().register_pending_materialization(
        P0,
        PersistentAxisMaterialization::DriveSequence {
            sequence,
            collapsed_axes: collapsed_axes.clone(),
        },
    );

    // (4) REACH-GUARD ON THE SEAM: the collapse authority really does name BOTH axes, so a
    // schedule-keyed hide filter would have suppressed both rows below. Without this, (5) and (6)
    // could pass because the stash never reached the `DriveSequence` arm at all.
    // It names both BECAUSE OF THE GRAFT, not because production would produce it — a later
    // reader "simplifying" the graft to match production silently vacates the mana half of this
    // row (`unbounded_mark_kind` filters `Mana(_)` at the registration site).
    let state = rig.runner.state();
    let scheduled = state.scheduled_collapse_axes(
        state
            .pending_unbounded_materialization
            .get(&P0)
            .expect("the grafted stash is present"),
    );
    assert!(
        scheduled.contains(&ResourceAxis::Mana(ManaType::Colorless))
            && scheduled.contains(&ResourceAxis::Life(P0)),
        "reach-guard: scheduled_collapse_axes returns BOTH axes unfiltered (this graft's stash \
         names the mana axis, so the clear still removes it here; production no longer names it — \
         ResourceAxis::unbounded_mark_kind), got {scheduled:?}"
    );

    for viewer in [None, Some(P0), Some(P1)] {
        let views = engine::game::derived_views::derive_views(state, viewer);
        let rows = views.unbounded_resources;
        let families = views.unbounded_families;
        let axes: Vec<ResourceAxis> = rows.iter().map(|r| r.axis).collect();
        // (5) the already-materialized mana axis keeps its ∞ row on the WIRE.
        assert!(
            axes.contains(&ResourceAxis::Mana(ManaType::Colorless)),
            "CR 500.5: mana is already in the pool and still being refilled, so a \
             merely-scheduled drive must NOT hide its ∞ row (viewer {viewer:?}), got {axes:?}"
        );
        // (6) DISCRIMINATOR — the DEFERRED axis of the SAME `DriveSequence` also keeps its ∞ row.
        // Nothing has been applied yet, so both rows project even though the collapse authority
        // names both axes at (4).
        assert!(
            axes.contains(&ResourceAxis::Life(P0)),
            "the deferred Life axis of the same scheduled drive still projects its ∞ \
             row while the collapse is merely scheduled (viewer {viewer:?}), got {axes:?}"
        );

        // (8) R4 — the documented `Mana(_)` scope limit is FALSIFIABLE, not dead code: (4) above
        // proves the collapse authority names BOTH axes on this exact stash, so the mana axis
        // going unflagged below can only come from the projection's own guard. Assertion (5)
        // already pins that the mana ROW still exists, so the scope limit governing the AFFORDANCE
        // rather than row EXISTENCE is covered there; a duplicate pin here would be subsumed by it
        // and by the same `derive_views` output, so this reuses `rows` instead of recomputing.

        // (9) R4/agree — the FAMILY COLLAPSE STATE obeys the `Mana(_)` scope limit.
        //
        // MEASURED DEFECT this pins: the limit once lived in a separate tag channel's loop and not
        // in the row loop, so on this exact state the mana row shipped `scheduled: true`. The HUD
        // folded that flag into the "mana" family and rendered `∞→N` with a "collapse pending; a
        // finite amount will be chosen" tooltip — beside a pool `refill_infinite_mana` is still
        // topping up, and beside `ManaPoolSummary`'s plain `∞` for the same pool in the same
        // frame. The whole suite was green over it: every other schedule assertion in the repo
        // sits on a non-mana axis, so nothing chose between that behaviour and its opposite. The
        // tag channel is gone and so is the row flag; this assertion is what keeps the scope limit
        // honest on the channel that replaced them.
        //
        // TWO-SIDED on purpose. The `life` half is the matched positive, from the SAME stash and
        // the SAME `derive_views` call: without it, `Unscheduled` everywhere satisfies the mana
        // half, and this row would pass against a channel that can never report a schedule.
        let state_of = |want: UnboundedFamily| {
            families
                .iter()
                .find(|f| f.player == P0 && f.family == want)
                .unwrap_or_else(|| panic!("R4/agree reach: no {want:?} family (viewer {viewer:?})"))
                .state
        };
        assert_eq!(
            state_of(UnboundedFamily::Mana),
            FamilyCollapseState::Unscheduled,
            "R4/agree: the mana family must not report a schedule — the accepted count bounds \
             nothing the player can spend (viewer {viewer:?})"
        );
        assert_eq!(
            state_of(UnboundedFamily::Life),
            FamilyCollapseState::Scheduled {
                certainty: CollapseCertainty::Committed,
                prompted: Some(P0),
            },
            "R4/agree positive: the deferred life family of the SAME stash IS scheduled, so the \
             mana assertion above is discriminating rather than vacuous. It is COMMITTED because \
             a `DriveSequence` replays real cycles and has no non-push exit (viewer {viewer:?})"
        );
    }

    // (7) THE STORE IS UNTOUCHED — the projection read, it did not mutate. The boundary
    // clear still reads both axes from here.
    assert_eq!(
        state
            .unbounded_resources
            .get(&P0)
            .map(|a| a.iter().copied().collect::<Vec<_>>()),
        Some(collapsed_axes),
        "the ∞ store survives the projection (engine-state enabler lockstep)"
    );
}

/// CR 611.3a + CR 732.2a: Basalt Monolith + Power Artifact beside a Professor
/// Hojo that isn't on the battlefield, in `zone`. Drives one period and
/// returns whether the mana-engine shortcut was offered. Reach guards: Hojo
/// (its Oracle text) carries its once-per-turn modifier in that zone, and the
/// period's untap really was journaled.
fn mana_engine_offered_beside_hojo_in(zone: Zone) -> bool {
    use engine::types::statics::{CastFrequency, StaticMode};
    // Scryfall Oracle text; the shared test database has no Hojo.
    const HOJO: &str = "The first activated ability you activate during your turn that targets a creature you control costs {2} less to activate.\nWhenever one or more creatures you control become the target of an activated ability, draw a card. This ability triggers only once each turn.";
    let db = shared_card_db().expect("the shared card database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let basalt = scenario.add_real_card(P0, BASALT, Zone::Battlefield, db);
    let hojo = scenario
        .add_creature_to_hand_from_oracle(P0, "Professor Hojo", 2, 2, HOJO)
        .id();
    let mut runner = scenario.build();
    if zone != Zone::Hand {
        remove_from_zone(runner.state_mut(), hojo, Zone::Hand, P0);
        add_to_zone(runner.state_mut(), hojo, zone, P0);
        runner.state_mut().objects.get_mut(&hojo).unwrap().zone = zone;
    }
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let power = place_on_battlefield(runner.state_mut(), P0, POWER, db);
    attach_to(runner.state_mut(), power, basalt);
    assert_eq!(runner.state().objects[&hojo].zone, zone);
    assert!(
        runner.state().objects[&hojo]
            .static_definitions
            .iter_unchecked()
            .any(|def| matches!(
                def.mode,
                StaticMode::ReduceAbilityCost {
                    frequency: Some(CastFrequency::OncePerTurn),
                    ..
                }
            )),
        "reach guard: Hojo carries its first-activation modifier in {zone:?}"
    );
    let mut rig = Rig { runner, basalt };
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).expect("mana ability");
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).expect("untap ability");

    drive_one_period(&mut rig, mana_idx, untap_idx);

    assert!(
        rig.runner
            .state()
            .abilities_activated_this_turn_by_player
            .get(&P0)
            .is_some_and(|rows| rows.iter().any(|row| row.source == basalt)),
        "reach guard: the untap was journaled"
    );
    matches!(
        rig.runner.state().waiting_for,
        WaitingFor::LoopShortcut { .. }
    )
}

/// End-to-end guard for a HIDDEN library Hojo. The P7 cover compares frames
/// built from `proposer_hidden_view`, which blanks the library card's statics,
/// so its comparator never sees this Hojo (probed: the reader is live on the
/// board and absent from every compared frame). This does not show the path
/// ignores the journal; it pins that a hidden Hojo never withholds the offer.
#[test]
fn a_hidden_library_hojo_does_not_block_the_mana_engine_shortcut() {
    if shared_card_db().is_none() {
        return;
    }
    assert!(mana_engine_offered_beside_hojo_in(Zone::Library));
}

/// CR 611.3a + CR 732.2a: a REVEALED Hojo (in the graveyard, which the cover's
/// frames keep) is seen by the comparator, which does gate on the journal.
/// Basalt's untap targets no creature, so no row qualifies for Hojo, the
/// per-definition projection keeps none, and the shortcut is still offered.
/// Keeping every row while any dormant reader exists (45fdbed7d's rule)
/// withholds it.
#[test]
fn a_revealed_hojo_does_not_block_the_mana_engine_shortcut() {
    if shared_card_db().is_none() {
        return;
    }
    assert!(mana_engine_offered_beside_hojo_in(Zone::Graveyard));
}

// ---------------------------------------------------------------------------
// Response-timed and "you may"-gated periods: Basalt Monolith + Rings of Brighthearth.
//
// CR 732.2a: a demonstrated loop period may contain a step taken while earlier objects are still
// on the stack (a mana-ability response; CR 605.3b, a mana ability uses no stack) and a
// resolution-time "you may" (CR 603.5) the loop controller answered. Each recorded step carries
// the stack length it was taken at (`stack_depth`), so the drive replays a response at the depth it
// was demonstrated at; the controller's own "may" answer is recorded as a `MayChoice` pin on the
// step it was asked during, and the drive replays it.
// ---------------------------------------------------------------------------

const RINGS: &str = "Rings of Brighthearth";
const GRIM: &str = "Grim Monolith";
const BOLT: &str = "Lightning Bolt";
/// A non-optional, free copier: isolates the response-timed step from any "may" prompt.
const COPIER: &str =
    "Whenever you activate an ability, if it isn't a mana ability, copy that ability.";
/// The proposer's own costless resolution-time "may".
const MAY_GAIN_ONE: &str =
    "Whenever you activate an ability, if it isn't a mana ability, you may gain 1 life.";
/// The opponent's resolution-time "may", triggered by the proposer's activations.
const OPPONENT_MAY_GAIN_ONE: &str =
    "Whenever an opponent activates an ability, if it isn't a mana ability, you may gain 1 life.";
/// A costed "may": declining keeps the mana, accepting spends more than a lap makes.
const MAY_PAY_FIVE: &str = "Whenever you activate an ability, if it isn't a mana ability, you may pay {5}. If you do, you gain 1 life.";
/// One source asking two "may" questions in one resolution.
const TWO_MAYS: &str = "Whenever you activate an ability, if it isn't a mana ability, you may gain 1 life. Then you may gain 2 life.";
/// A token maker: its activation seeds a period on its own.
const SERVO_MAKER: &str = "{1}: Create a 1/1 colorless Servo artifact creature token.";

/// One board for the response-timed / "may"-gated period tests. Every named card is a real
/// fixture card; the synthetic texts are artifacts built from Oracle text.
#[derive(Clone, Copy)]
struct LapBoard {
    rock: &'static str,
    power: bool,
    rings: bool,
    synthetic: &'static [&'static str],
    opponent_synthetic: &'static [&'static str],
    colorless_seed: usize,
    rock_tapped: bool,
    bolts: usize,
    mountains: usize,
}

impl LapBoard {
    /// A bare, untapped Basalt Monolith on P0's battlefield; each test overrides what it needs.
    const BARE: Self = Self {
        rock: BASALT,
        power: false,
        rings: false,
        synthetic: &[],
        opponent_synthetic: &[],
        colorless_seed: 0,
        rock_tapped: false,
        bolts: 0,
        mountains: 0,
    };
}

struct LapRig {
    runner: GameRunner,
    rock: ObjectId,
    rings: Option<ObjectId>,
    mana_idx: usize,
    untap_idx: usize,
}

/// Build `board` under `LoopDetectionMode::Interactive`. Power Artifact is attached to the rock
/// after build (the aura attach-choice bypass); each Lightning Bolt gets one seeded red unit.
fn lap_rig(board: LapBoard) -> LapRig {
    let db = shared_card_db().expect("committed fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rock = scenario.add_real_card(P0, board.rock, Zone::Battlefield, db);
    let rings = board
        .rings
        .then(|| scenario.add_real_card(P0, RINGS, Zone::Battlefield, db));
    for (i, text) in board.synthetic.iter().enumerate() {
        scenario.add_artifact_from_oracle(P0, &format!("Synthetic {i}"), text);
    }
    for (i, text) in board.opponent_synthetic.iter().enumerate() {
        scenario.add_artifact_from_oracle(P1, &format!("Opponent synthetic {i}"), text);
    }
    for _ in 0..board.bolts {
        scenario.add_real_card(P0, BOLT, Zone::Hand, db);
    }
    for _ in 0..board.mountains {
        scenario.add_basic_land(P0, engine::types::mana::ManaColor::Red);
    }
    let units: Vec<ManaUnit> =
        (0..board.colorless_seed)
            .map(|i| {
                ManaUnit::new(
                    ManaType::Colorless,
                    ObjectId(9_900 + i as u64),
                    false,
                    Vec::new(),
                )
            })
            .chain((0..board.bolts).map(|i| {
                ManaUnit::new(ManaType::Red, ObjectId(9_800 + i as u64), false, Vec::new())
            }))
            .collect();
    if !units.is_empty() {
        scenario.with_mana_pool(P0, units);
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    if board.power {
        let power = place_on_battlefield(runner.state_mut(), P0, POWER, db);
        attach_to(runner.state_mut(), power, rock);
        assert_eq!(
            runner.state().objects[&power].attached_to,
            Some(rock.into()),
            "Power Artifact must attach to the rock"
        );
    }
    if board.rock_tapped {
        runner
            .state_mut()
            .objects
            .get_mut(&rock)
            .expect("the rock is on the battlefield")
            .tapped = true;
    }
    let mana_idx = mana_ability_index(runner.state(), rock).expect("the rock's mana ability");
    let untap_idx = untap_ability_index(runner.state(), rock).expect("the rock's untap ability");
    LapRig {
        runner,
        rock,
        rings,
        mana_idx,
        untap_idx,
    }
}

/// How a "you may" prompt of a lap is answered (by whichever seat the prompt names).
#[derive(Clone, Copy)]
enum MayAnswer {
    Take,
    Decline,
    /// Accept and remember the answer for this exact trigger instance.
    RememberTake,
}

/// One observed "you may" prompt.
#[derive(Clone, Copy, Debug)]
struct ObservedPrompt {
    player: PlayerId,
    source_id: ObjectId,
    keyed: bool,
    sequence_len_before: usize,
    sequence_len_after: usize,
}

/// What one human lap observed.
#[derive(Debug, Default)]
struct LapRecord {
    /// The stack length immediately before each activation of the lap, in order.
    depths: Vec<usize>,
    prompts: Vec<ObservedPrompt>,
}

/// One human lap: tap the rock for mana if it is untapped, activate its untap, then answer every
/// "you may" prompt from `answers` (an unlisted prompt takes) and, when `respond` is set, tap the
/// rock again in response to the original untap (once the stack is one above the untap's base and
/// the rock is untapped again). Passes priority until the stack is empty or an offer surfaces.
fn human_lap(rig: &mut LapRig, answers: &[MayAnswer], respond: bool) -> LapRecord {
    let mut record = LapRecord::default();
    let runner = &mut rig.runner;
    if !runner.state().objects[&rig.rock].tapped {
        record.depths.push(runner.state().stack.len());
        runner
            .act(GameAction::ActivateAbility {
                source_id: rig.rock,
                ability_index: rig.mana_idx,
            })
            .expect("tap the rock for mana");
    }
    let base = runner.state().stack.len();
    record.depths.push(base);
    runner
        .act(GameAction::ActivateAbility {
            source_id: rig.rock,
            ability_index: rig.untap_idx,
        })
        .expect("activate the rock's untap");
    let mut responded = false;
    let mut answered = 0;
    for _ in 0..60 {
        let state = runner.state();
        let (action, answering) = match &state.waiting_for {
            WaitingFor::LoopShortcut { .. } => break,
            WaitingFor::OptionalEffectChoice {
                player,
                source_id,
                may_trigger_key,
                ..
            } => {
                record.prompts.push(ObservedPrompt {
                    player: *player,
                    source_id: *source_id,
                    keyed: may_trigger_key.is_some(),
                    sequence_len_before: state.last_loop_action_sequence.len(),
                    sequence_len_after: 0,
                });
                let answer = answers.get(answered).copied().unwrap_or(MayAnswer::Take);
                answered += 1;
                let action = match answer {
                    MayAnswer::Take => GameAction::DecideOptionalEffect { accept: true },
                    MayAnswer::Decline => GameAction::DecideOptionalEffect { accept: false },
                    MayAnswer::RememberTake => GameAction::DecideOptionalEffectAndRemember {
                        choice: engine::types::game_state::AutoMayChoice::Accept,
                        scope: engine::types::game_state::MayTriggerAutoChoiceScope::ExactInstance,
                    },
                };
                (action, true)
            }
            WaitingFor::Priority { .. } if state.stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                if respond
                    && !responded
                    && state.stack.len() == base + 1
                    && !state.objects[&rig.rock].tapped
                {
                    responded = true;
                    record.depths.push(state.stack.len());
                    (
                        GameAction::ActivateAbility {
                            source_id: rig.rock,
                            ability_index: rig.mana_idx,
                        },
                        false,
                    )
                } else {
                    (GameAction::PassPriority, false)
                }
            }
            other => panic!("unexpected prompt during the lap: {other:?}"),
        };
        runner.act(action).expect("a legal lap action");
        if answering {
            let len = runner.state().last_loop_action_sequence.len();
            record
                .prompts
                .last_mut()
                .expect("the prompt was just recorded")
                .sequence_len_after = len;
        }
    }
    record
}

/// Pass priority until the stack is empty at a priority window or an offer surfaces.
fn settle_lap(runner: &mut GameRunner) {
    for _ in 0..60 {
        match &runner.state().waiting_for {
            WaitingFor::LoopShortcut { .. } => return,
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while settling: {other:?}"),
        }
    }
    panic!("the stack never settled");
}

/// Pass priority until the stack holds exactly `len` objects at a priority window.
fn pass_until_stack_len(runner: &mut GameRunner, len: usize) {
    for _ in 0..30 {
        if runner.state().stack.len() == len
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    panic!("the stack never reached {len} objects");
}

/// Cast a Lightning Bolt from P0's hand at P1 and keep it on the stack (P0 keeps priority).
fn cast_bolt_at_opponent(runner: &mut GameRunner) {
    let bolt = runner
        .state()
        .objects
        .iter()
        .filter(|(_, o)| o.name == BOLT && o.zone == Zone::Hand)
        .map(|(id, _)| *id)
        .min()
        .expect("a Lightning Bolt in hand");
    let card_id = runner.state().objects[&bolt].card_id;
    let before = runner.state().stack.len();
    let result = runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Lightning Bolt");
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            })
            .expect("target the opponent");
    }
    assert_eq!(
        runner.state().stack.len(),
        before + 1,
        "the Bolt is on the stack"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "the caster keeps priority with the Bolt pending"
    );
}

fn offered(runner: &GameRunner) -> bool {
    matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { .. })
}

/// `(ability_index, stack_depth)` of every recorded period step, in order.
fn recorded_steps(runner: &GameRunner) -> Vec<(usize, usize)> {
    runner
        .state()
        .last_loop_action_sequence
        .iter()
        .map(|step| match step.action {
            LoopAction::Activate { ability_index, .. } => (ability_index, step.stack_depth),
            ref other => panic!("expected an Activate step, got {other:?}"),
        })
        .collect()
}

fn pins_per_step(runner: &GameRunner) -> Vec<usize> {
    runner
        .state()
        .last_loop_action_sequence
        .iter()
        .map(|step| step.pins.len())
        .collect()
}

/// The `MayChoice` answers pinned on step `index`, with every pin's source; panics on a pin of
/// another kind.
fn may_pins(
    runner: &GameRunner,
    index: usize,
) -> Vec<(
    engine::types::game_state::YieldTarget,
    engine::analysis::decision_template::MayChoiceOption,
)> {
    use engine::analysis::decision_template::PinnedDecision;
    runner.state().last_loop_action_sequence[index]
        .pins
        .iter()
        .map(|pin| match pin {
            PinnedDecision::MayChoice { slot, take } => (slot.source.clone(), *take),
            other => panic!("expected only MayChoice pins, got {other:?}"),
        })
        .collect()
}

/// The source object a `MayChoice` slot names.
fn slot_object(source: &engine::types::game_state::YieldTarget) -> Option<ObjectId> {
    match source {
        engine::types::game_state::YieldTarget::ThisObject { source_id, .. } => Some(*source_id),
        _ => None,
    }
}

/// The offer's unbounded axes and published schema-point count; panics without an offer.
fn offer_shape(runner: &GameRunner) -> (Vec<ResourceAxis>, usize) {
    match &runner.state().waiting_for {
        WaitingFor::LoopShortcut {
            proposer,
            certificate,
            schema,
            ..
        } => {
            assert_eq!(*proposer, P0, "the loop controller proposes the shortcut");
            (certificate.unbounded.clone(), schema.points.len())
        }
        other => panic!("expected a CR 732.2a LoopShortcut offer, got {other:?}"),
    }
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

/// ⭐ CR 732.2a + CR 603.5 + CR 707.10: real Basalt Monolith + real Rings of Brighthearth. One human
/// lap (tap for three, pay {3} to untap, pay Rings' {2} to copy the untap, tap again in response
/// to the original untap) nets +1 colorless and returns the board to an untapped Basalt with an
/// empty stack, so the controller is OFFERED the mana shortcut. The period records the response
/// tap at depth 1 and the Rings answer as a `Take` pin on the untap step only (never the first
/// step, so the offer publishes no schema point). Revert-failing: replaying every step to the
/// empty stack (no depth settle), dropping the drive's "may" arm, or dropping the pin each lose
/// the offer.
#[test]
fn mana_engine_basalt_rings_offers_mana_advantage_shortcut() {
    use engine::analysis::decision_template::MayChoiceOption;
    let mut rig = lap_rig(LapBoard {
        rings: true,
        colorless_seed: 2,
        ..LapBoard::BARE
    });
    let rings = rig.rings.expect("Rings is on the battlefield");
    let lap = human_lap(&mut rig, &[], true);

    // Reach guards: Rings asked P0 once, and the response tap happened over the original untap.
    assert_eq!(lap.prompts.len(), 1, "Rings asked its question once");
    assert_eq!(lap.prompts[0].player, P0);
    assert_eq!(lap.prompts[0].source_id, rings);
    assert_eq!(
        lap.prompts[0].sequence_len_before, 2,
        "Rings asks while the untap is the current step"
    );
    assert_eq!(lap.depths, vec![0, 0, 1], "tap, untap, response tap");
    assert_eq!(
        recorded_steps(&rig.runner),
        vec![(rig.mana_idx, 0), (rig.untap_idx, 0), (rig.mana_idx, 1)],
        "the period is [tap@0, untap@0, tap@1] with absolute depths"
    );
    assert!(may_pins(&rig.runner, 0).is_empty() && may_pins(&rig.runner, 2).is_empty());
    let untap_pins = may_pins(&rig.runner, 1);
    assert_eq!(untap_pins.len(), 1, "exactly one pin on the untap step");
    assert_eq!(slot_object(&untap_pins[0].0), Some(rings));
    assert_eq!(untap_pins[0].1, MayChoiceOption::Take);
    assert_eq!(colorless(rig.runner.state(), P0), 3, "the lap nets +1");
    match &rig.runner.state().waiting_for {
        WaitingFor::LoopShortcut {
            proposer,
            certificate,
            schema,
            ..
        } => {
            assert_eq!(*proposer, P0);
            assert_eq!(
                certificate.unbounded,
                vec![ResourceAxis::Mana(ManaType::Colorless)]
            );
            assert_eq!(certificate.win_kind, WinKind::Advantage);
            assert!(
                schema.points.is_empty(),
                "the demonstrated Rings answer is not a published shortcut parameter"
            );
        }
        other => panic!("expected a CR 732.2a LoopShortcut offer, got {other:?}"),
    }
}

/// CR 732.2a + CR 732.2c: accepting the Basalt + Rings offer grants infinite colorless mana, exactly
/// as the shipped Basalt + Power accept does.
#[test]
fn mana_engine_basalt_rings_accept_marks_infinite_mana() {
    let mut rig = lap_rig(LapBoard {
        rings: true,
        colorless_seed: 2,
        ..LapBoard::BARE
    });
    human_lap(&mut rig, &[], true);
    assert!(offered(&rig.runner), "precondition: the offer fired");
    assert_eq!(colorless(rig.runner.state(), P0), 3);
    rig.runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(5),
            template: None,
        })
        .expect("declare shortcut");
    assert!(
        matches!(
            rig.runner.state().waiting_for,
            WaitingFor::RespondToShortcut { .. }
        ),
        "the opponent gets the accept-or-shorten window"
    );
    rig.runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("opponent accepts");
    let state = rig.runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(
        state
            .unbounded_resources
            .get(&P0)
            .is_some_and(|axes| axes.contains(&ResourceAxis::Mana(ManaType::Colorless))),
        "the colorless axis is marked unbounded"
    );
    assert!(
        colorless(state, P0) >= 90,
        "the pool is topped to the infinite-mana constant, got {}",
        colorless(state, P0)
    );
    assert!(state.stack.is_empty());
    assert!(!state.objects[&rig.rock].tapped, "Basalt ends untapped");
}

/// CR 603.5 + CR 732.2a: a DECLINED Rings answer is recorded as demonstrated (a `Decline` pin on
/// the untap step). The declined lap nets 0, so there is no offer either way; only the pin value
/// discriminates a recorder that ignores the answer (a constant `Take`). Paired positive: R1.
#[test]
fn mana_engine_rings_decline_records_decline_pin() {
    use engine::analysis::decision_template::MayChoiceOption;
    let mut rig = lap_rig(LapBoard {
        rings: true,
        colorless_seed: 2,
        ..LapBoard::BARE
    });
    let rings = rig.rings.expect("Rings is on the battlefield");
    let lap = human_lap(&mut rig, &[MayAnswer::Decline], true);

    assert_eq!(lap.prompts.len(), 1, "reach guard: Rings asked");
    assert_eq!(
        recorded_steps(&rig.runner),
        vec![(rig.mana_idx, 0), (rig.untap_idx, 0)],
        "no copy, so no response tap"
    );
    let pins = may_pins(&rig.runner, 1);
    assert_eq!(pins.len(), 1);
    assert_eq!(slot_object(&pins[0].0), Some(rings));
    assert_eq!(
        pins[0].1,
        MayChoiceOption::Decline,
        "the demonstrated answer"
    );
    assert_eq!(
        colorless(rig.runner.state(), P0),
        2,
        "a declined lap nets 0"
    );
    assert!(!offered(&rig.runner));
}

/// CR 732.2a: Grim Monolith + Rings nets 0 (+3 -4 -2 +3), so there is no shortcut. Same lap shape,
/// prompt and recorded depths as the offered boards: the rejection is the sign-check, not a drive
/// abort. Paired positive: `mana_engine_grim_power_rings_offers`.
#[test]
fn mana_engine_grim_rings_net_zero_no_offer() {
    let mut rig = lap_rig(LapBoard {
        rock: GRIM,
        rings: true,
        colorless_seed: 6,
        ..LapBoard::BARE
    });
    let lap = human_lap(&mut rig, &[], true);
    assert_eq!(lap.prompts.len(), 1, "reach guard: Rings asked");
    let depths: Vec<usize> = recorded_steps(&rig.runner).iter().map(|s| s.1).collect();
    assert_eq!(depths, vec![0, 0, 1]);
    assert_eq!(colorless(rig.runner.state(), P0), 6, "the lap nets 0");
    assert!(!offered(&rig.runner), "a net-0 lap is not a loop");
}

/// Control for R4: Grim Monolith + attached Power Artifact + Rings nets +2 (+3 -2 -2 +3) and is
/// OFFERED.
#[test]
fn mana_engine_grim_power_rings_offers() {
    let mut rig = lap_rig(LapBoard {
        rock: GRIM,
        power: true,
        rings: true,
        colorless_seed: 6,
        ..LapBoard::BARE
    });
    let lap = human_lap(&mut rig, &[], true);
    assert_eq!(lap.prompts.len(), 1, "reach guard: Rings asked");
    let depths: Vec<usize> = recorded_steps(&rig.runner).iter().map(|s| s.1).collect();
    assert_eq!(depths, vec![0, 0, 1]);
    assert_eq!(colorless(rig.runner.state(), P0), 8, "the lap nets +2");
    assert_eq!(
        offer_shape(&rig.runner).0,
        vec![ResourceAxis::Mana(ManaType::Colorless)]
    );
}

/// CR 732.2a: two cost sources compose. Basalt + Power + Rings nets +3 (+3 -1 -2 +3) and is OFFERED.
#[test]
fn mana_engine_basalt_power_rings_offers() {
    let mut rig = lap_rig(LapBoard {
        power: true,
        rings: true,
        colorless_seed: 6,
        ..LapBoard::BARE
    });
    let lap = human_lap(&mut rig, &[], true);
    assert_eq!(lap.prompts.len(), 1, "reach guard: Rings asked");
    let depths: Vec<usize> = recorded_steps(&rig.runner).iter().map(|s| s.1).collect();
    assert_eq!(depths, vec![0, 0, 1]);
    assert_eq!(colorless(rig.runner.state(), P0), 9, "the lap nets +3");
    assert_eq!(
        offer_shape(&rig.runner).0,
        vec![ResourceAxis::Mana(ManaType::Colorless)]
    );
}

/// CR 603.5 + CR 732.2a: a REMEMBERED Rings answer is a standing instruction the drive's state clone
/// carries, so the lap is OFFERED with no `MayChoice` pin on any step.
#[test]
fn mana_engine_rings_remembered_accept_offers_without_a_may_pin() {
    let mut rig = lap_rig(LapBoard {
        rings: true,
        colorless_seed: 2,
        ..LapBoard::BARE
    });
    let lap = human_lap(&mut rig, &[MayAnswer::RememberTake], true);
    assert_eq!(lap.prompts.len(), 1);
    assert!(
        lap.prompts[0].keyed,
        "reach guard: the prompt is rememberable"
    );
    assert_eq!(rig.runner.state().may_trigger_auto_choices.len(), 1);
    assert_eq!(pins_per_step(&rig.runner), vec![0, 0, 0]);
    assert_eq!(
        offer_shape(&rig.runner).0,
        vec![ResourceAxis::Mana(ManaType::Colorless)]
    );
}

/// ⭐ CR 732.2a: a player who STARTS with Basalt tapped plays an opening lap (untap, Rings copy,
/// response tap: -2) and then loops. The opening lap leaves a lone response tap recorded (its
/// untap was never recorded); the second lap's tap at a lower depth replaces that orphan, so the
/// second lap records EXACTLY the untapped-start period and is OFFERED. Revert-failing: without the
/// orphan replacement the second lap records a four-step sequence and is not offered.
#[test]
fn mana_engine_tapped_start_rings_offers_on_the_second_lap() {
    let mut rig = lap_rig(LapBoard {
        rings: true,
        colorless_seed: 7,
        rock_tapped: true,
        ..LapBoard::BARE
    });
    let opening = human_lap(&mut rig, &[], true);
    assert_eq!(opening.prompts.len(), 1, "reach guard: Rings asked");
    assert_eq!(opening.depths, vec![0, 1], "untap, then response tap");
    assert_eq!(
        recorded_steps(&rig.runner),
        vec![(rig.mana_idx, 1)],
        "reach guard: the orphan response tap is the lone recorded step"
    );
    assert_eq!(
        colorless(rig.runner.state(), P0),
        5,
        "the opening lap nets -2"
    );
    assert!(!offered(&rig.runner));

    let second = human_lap(&mut rig, &[], true);
    assert_eq!(second.prompts.len(), 1);
    assert_eq!(
        recorded_steps(&rig.runner),
        vec![(rig.mana_idx, 0), (rig.untap_idx, 0), (rig.mana_idx, 1)]
    );
    assert_eq!(
        colorless(rig.runner.state(), P0),
        6,
        "the second lap nets +1"
    );
    assert_eq!(
        offer_shape(&rig.runner).0,
        vec![ResourceAxis::Mana(ManaType::Colorless)]
    );

    let mut untapped_start = lap_rig(LapBoard {
        rings: true,
        colorless_seed: 2,
        ..LapBoard::BARE
    });
    human_lap(&mut untapped_start, &[], true);
    assert_eq!(
        rig.runner.state().last_loop_action_sequence,
        untapped_start.runner.state().last_loop_action_sequence,
        "the second tapped-start lap records the untapped-start period"
    );
}

fn mountains_tapped(runner: &GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            let o = &runner.state().objects[id];
            o.name == "Mountain" && o.tapped
        })
        .count()
}

/// CR 732.2a: the natural board — Mountains and an EMPTY pool, Rings' {2} paid by auto-tapped lands
/// when the pool cannot — is offered too: (a) untapped Basalt with three Mountains; (b) tapped
/// Basalt with five Mountains, offered on the second lap; (c) with Power Artifact, where the
/// freshly tapped Basalt mana pays everything.
#[test]
fn mana_engine_land_funded_empty_pool_rings_offers() {
    // (a) untapped start: tap +3, untap -3 from the pool, Rings {2} from two Mountains, tap +3.
    let mut rig = lap_rig(LapBoard {
        rings: true,
        mountains: 3,
        ..LapBoard::BARE
    });
    let lap = human_lap(&mut rig, &[], true);
    assert_eq!(lap.prompts.len(), 1, "(a) reach guard: Rings asked");
    let depths: Vec<usize> = recorded_steps(&rig.runner).iter().map(|s| s.1).collect();
    assert_eq!(depths, vec![0, 0, 1], "(a)");
    assert_eq!(colorless(rig.runner.state(), P0), 3, "(a)");
    assert_eq!(
        mountains_tapped(&rig.runner),
        2,
        "(a) two Mountains paid Rings"
    );
    assert_eq!(
        offer_shape(&rig.runner),
        (vec![ResourceAxis::Mana(ManaType::Colorless)], 0),
        "(a)"
    );

    // (b) tapped start: the opening lap is paid by all five Mountains, the second is offered.
    let mut rig = lap_rig(LapBoard {
        rings: true,
        mountains: 5,
        rock_tapped: true,
        ..LapBoard::BARE
    });
    let opening = human_lap(&mut rig, &[], true);
    assert_eq!(opening.prompts.len(), 1, "(b) reach guard: Rings asked");
    assert_eq!(recorded_steps(&rig.runner), vec![(rig.mana_idx, 1)], "(b)");
    assert_eq!(colorless(rig.runner.state(), P0), 3, "(b)");
    assert_eq!(mountains_tapped(&rig.runner), 5, "(b)");
    assert!(!offered(&rig.runner), "(b) the opening lap is not offered");
    let second = human_lap(&mut rig, &[], true);
    assert_eq!(
        second.prompts.len(),
        1,
        "(b) reach guard: Rings asked again"
    );
    let depths: Vec<usize> = recorded_steps(&rig.runner).iter().map(|s| s.1).collect();
    assert_eq!(depths, vec![0, 0, 1], "(b)");
    assert_eq!(colorless(rig.runner.state(), P0), 4, "(b)");
    assert_eq!(
        offer_shape(&rig.runner),
        (vec![ResourceAxis::Mana(ManaType::Colorless)], 0),
        "(b)"
    );

    // (c) Power Artifact: the untap costs {1}; Basalt's own mana pays it and Rings.
    let mut rig = lap_rig(LapBoard {
        power: true,
        rings: true,
        mountains: 3,
        ..LapBoard::BARE
    });
    let lap = human_lap(&mut rig, &[], true);
    assert_eq!(lap.prompts.len(), 1, "(c) reach guard: Rings asked");
    assert_eq!(colorless(rig.runner.state(), P0), 3, "(c)");
    assert_eq!(mountains_tapped(&rig.runner), 0, "(c) no land was needed");
    assert_eq!(
        offer_shape(&rig.runner),
        (vec![ResourceAxis::Mana(ManaType::Colorless)], 0),
        "(c)"
    );
}

/// CR 732.2a + CR 707.10: response timing with no prompt. Basalt + a non-optional free copier: tap,
/// untap (copied), tap again in response to the original: +3 per lap, OFFERED. The response tap
/// must be replayed at depth 1; replayed to the empty stack, the second untap is wasted and the
/// cover fails.
#[test]
fn mana_engine_response_timed_copier_offers() {
    let mut rig = lap_rig(LapBoard {
        synthetic: &[COPIER],
        colorless_seed: 2,
        ..LapBoard::BARE
    });
    let lap = human_lap(&mut rig, &[], true);
    assert!(lap.prompts.is_empty(), "the copier asks nothing");
    assert_eq!(
        lap.depths,
        vec![0, 0, 1],
        "reach guard: the response tap happened"
    );
    assert_eq!(
        recorded_steps(&rig.runner),
        vec![(rig.mana_idx, 0), (rig.untap_idx, 0), (rig.mana_idx, 1)]
    );
    assert_eq!(colorless(rig.runner.state(), P0), 5, "the lap nets +3");
    assert_eq!(
        offer_shape(&rig.runner).0,
        vec![ResourceAxis::Mana(ManaType::Colorless)]
    );
}

/// CR 732.2a: a recorded depth the replay cannot reach (a mana ability never leaves an object on
/// the stack) degrades to settling on the empty stack instead of losing a period that replays
/// that way. Control: the same lap untampered is offered.
#[test]
fn mana_engine_unreproducible_depth_degrades_to_settle_to_empty() {
    for tamper in [false, true] {
        let mut rig = lap_rig(LapBoard {
            power: true,
            ..LapBoard::BARE
        });
        for ability_index in [rig.mana_idx, rig.untap_idx] {
            rig.runner
                .act(GameAction::ActivateAbility {
                    source_id: rig.rock,
                    ability_index,
                })
                .expect("a legal activation");
        }
        assert_eq!(
            recorded_steps(&rig.runner),
            vec![(rig.mana_idx, 0), (rig.untap_idx, 0)],
            "reach guard: both steps recorded at depth 0"
        );
        if tamper {
            // Step 0 now settles at depth 2, which its mana ability can never reach.
            rig.runner.state_mut().last_loop_action_sequence[1].stack_depth = 2;
        }
        settle_lap(&mut rig.runner);
        assert!(
            offered(&rig.runner),
            "tamper={tamper}: the period is still offered"
        );
        assert!(rig.runner.state().stack.is_empty());
    }
}

/// CR 732.2a: a period demonstrated over an unrelated pending object (the controller's own Bolt,
/// both steps taken over it) is still offered: depths are read relative to the first step.
#[test]
fn mana_engine_period_over_a_pending_spell_is_still_offered() {
    let mut rig = lap_rig(LapBoard {
        power: true,
        bolts: 1,
        ..LapBoard::BARE
    });
    cast_bolt_at_opponent(&mut rig.runner);
    let lap = human_lap(&mut rig, &[], false);
    settle_lap(&mut rig.runner);
    assert_eq!(lap.depths, vec![1, 1]);
    assert_eq!(
        recorded_steps(&rig.runner),
        vec![(rig.mana_idx, 1), (rig.untap_idx, 1)]
    );
    assert!(offered(&rig.runner));
    assert_eq!(life(&rig.runner, P1), 17, "reach guard: the Bolt resolved");
}

/// CR 732.2a: `[1, 0]` — the first step over a pending Bolt, the second after it resolved — is still
/// offered (relative depth saturates at 0), and the orphan replacement does not fire on a
/// differing follow-up action.
#[test]
fn mana_engine_period_first_step_over_a_spell_then_resolved_is_still_offered() {
    let mut rig = lap_rig(LapBoard {
        power: true,
        bolts: 1,
        ..LapBoard::BARE
    });
    cast_bolt_at_opponent(&mut rig.runner);
    rig.runner
        .act(GameAction::ActivateAbility {
            source_id: rig.rock,
            ability_index: rig.mana_idx,
        })
        .expect("tap in response to the Bolt");
    pass_until_stack_len(&mut rig.runner, 0);
    assert_eq!(life(&rig.runner, P1), 17, "reach guard: the Bolt resolved");
    rig.runner
        .act(GameAction::ActivateAbility {
            source_id: rig.rock,
            ability_index: rig.untap_idx,
        })
        .expect("untap");
    assert_eq!(
        recorded_steps(&rig.runner),
        vec![(rig.mana_idx, 1), (rig.untap_idx, 0)],
        "both steps recorded"
    );
    settle_lap(&mut rig.runner);
    assert!(offered(&rig.runner));
}

/// CR 732.2a: `[2, 1]` — two Bolts pending at the tap, one at the untap — is still offered.
#[test]
fn mana_engine_period_depth_two_then_one_is_still_offered() {
    let mut rig = lap_rig(LapBoard {
        power: true,
        bolts: 2,
        ..LapBoard::BARE
    });
    cast_bolt_at_opponent(&mut rig.runner);
    cast_bolt_at_opponent(&mut rig.runner);
    rig.runner
        .act(GameAction::ActivateAbility {
            source_id: rig.rock,
            ability_index: rig.mana_idx,
        })
        .expect("tap over two Bolts");
    pass_until_stack_len(&mut rig.runner, 1);
    rig.runner
        .act(GameAction::ActivateAbility {
            source_id: rig.rock,
            ability_index: rig.untap_idx,
        })
        .expect("untap over one Bolt");
    assert_eq!(
        recorded_steps(&rig.runner),
        vec![(rig.mana_idx, 2), (rig.untap_idx, 1)]
    );
    settle_lap(&mut rig.runner);
    assert!(offered(&rig.runner));
    assert_eq!(
        life(&rig.runner, P1),
        14,
        "reach guard: both Bolts resolved"
    );
}

/// CR 732.2a: the Rings lap taken over the controller's own pending Bolt is offered: the response
/// tap settles one object above the period's first step.
#[test]
fn mana_engine_rings_lap_over_a_pending_spell_offers() {
    let mut rig = lap_rig(LapBoard {
        rings: true,
        colorless_seed: 2,
        bolts: 1,
        ..LapBoard::BARE
    });
    cast_bolt_at_opponent(&mut rig.runner);
    let lap = human_lap(&mut rig, &[], true);
    settle_lap(&mut rig.runner);
    assert_eq!(lap.prompts.len(), 1, "reach guard: Rings asked");
    assert_eq!(lap.depths, vec![1, 1, 2]);
    assert_eq!(
        recorded_steps(&rig.runner),
        vec![(rig.mana_idx, 1), (rig.untap_idx, 1), (rig.mana_idx, 2)]
    );
    assert_eq!(colorless(rig.runner.state(), P0), 3, "the lap nets +1");
    assert_eq!(life(&rig.runner, P1), 17, "reach guard: the Bolt resolved");
    assert_eq!(
        offer_shape(&rig.runner).0,
        vec![ResourceAxis::Mana(ManaType::Colorless)]
    );
}

/// CR 603.5 + CR 732.2a: the pin follows the demonstrated answer in BOTH directions. Basalt, Power
/// and "you may pay {5}. If you do, you gain 1 life.": declined, the lap nets +2 and is OFFERED;
/// accepted, it nets -3 and is not. A recorder that pinned a constant `Take` loses the declined
/// offer.
#[test]
fn mana_engine_may_pay_pin_follows_demonstrated_answer() {
    use engine::analysis::decision_template::MayChoiceOption;
    for (answer, option, pool, p0_life, expect_offer) in [
        (MayAnswer::Decline, MayChoiceOption::Decline, 7, 20, true),
        (MayAnswer::Take, MayChoiceOption::Take, 2, 21, false),
    ] {
        let mut rig = lap_rig(LapBoard {
            power: true,
            synthetic: &[MAY_PAY_FIVE],
            colorless_seed: 5,
            ..LapBoard::BARE
        });
        let lap = human_lap(&mut rig, &[answer], false);
        assert_eq!(lap.prompts.len(), 1, "{option:?}: reach guard: asked");
        assert_eq!(lap.prompts[0].player, P0);
        assert_eq!(
            recorded_steps(&rig.runner),
            vec![(rig.mana_idx, 0), (rig.untap_idx, 0)]
        );
        assert!(may_pins(&rig.runner, 0).is_empty());
        let pins: Vec<MayChoiceOption> =
            may_pins(&rig.runner, 1).into_iter().map(|p| p.1).collect();
        assert_eq!(
            pins,
            vec![option],
            "the untap step pins the demonstrated answer"
        );
        assert_eq!(colorless(rig.runner.state(), P0), pool, "{option:?}");
        assert_eq!(life(&rig.runner, P0), p0_life, "{option:?}");
        assert_eq!(offered(&rig.runner), expect_offer, "{option:?}");
    }
}

/// CR 603.5 + CR 732.2a: one source asking two "may" questions on one step with the SAME answer is
/// replayed from one pin and OFFERED (life is unbounded too).
#[test]
fn mana_engine_duplicate_may_slot_same_answer_replays() {
    use engine::analysis::decision_template::MayChoiceOption;
    let mut rig = lap_rig(LapBoard {
        power: true,
        synthetic: &[TWO_MAYS],
        ..LapBoard::BARE
    });
    let lap = human_lap(&mut rig, &[MayAnswer::Take, MayAnswer::Take], false);
    assert_eq!(lap.prompts.len(), 2, "reach guard: asked twice");
    assert_eq!(lap.prompts[0].source_id, lap.prompts[1].source_id);
    let pins: Vec<MayChoiceOption> = may_pins(&rig.runner, 1).into_iter().map(|p| p.1).collect();
    assert_eq!(pins, vec![MayChoiceOption::Take], "one pin for the slot");
    assert_eq!(life(&rig.runner, P0), 23);
    assert_eq!(
        offer_shape(&rig.runner).0,
        vec![
            ResourceAxis::Mana(ManaType::Colorless),
            ResourceAxis::Life(P0)
        ]
    );
}

/// CR 603.5 + CR 732.2a: two DIFFERENT answers from one source on one step cannot be replayed from
/// one pin, so the period is discarded and nothing is offered.
#[test]
fn mana_engine_duplicate_may_slot_differing_answers_poison_the_period() {
    let mut rig = lap_rig(LapBoard {
        power: true,
        synthetic: &[TWO_MAYS],
        ..LapBoard::BARE
    });
    let lap = human_lap(&mut rig, &[MayAnswer::Take, MayAnswer::Decline], false);
    assert_eq!(lap.prompts.len(), 2, "reach guard: asked twice");
    assert_eq!(
        lap.prompts[0].source_id, lap.prompts[1].source_id,
        "both questions come from one source"
    );
    assert_eq!(
        lap.prompts[0].sequence_len_after, 2,
        "reach guard: the first answer kept the period"
    );
    assert_eq!(
        lap.prompts[1].sequence_len_after, 0,
        "the differing second answer discards the period"
    );
    assert!(rig.runner.state().last_loop_action_sequence.is_empty());
    assert_eq!(life(&rig.runner, P0), 21);
    assert!(!offered(&rig.runner));
}

/// CR 603.5 + CR 732.2a: a "may" answered while the period consists of its first step only attaches
/// no pin (the first step's pins are the ones an offer publishes) and keeps the period; one step
/// later the same answer is pinned.
#[test]
fn mana_engine_may_on_the_seed_step_is_not_pinned() {
    use engine::analysis::decision_template::MayChoiceOption;
    for with_basalt_first in [false, true] {
        let db = shared_card_db().expect("committed fixture");
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let basalt = scenario.add_real_card(P0, BASALT, Zone::Battlefield, db);
        let maker = scenario
            .add_artifact_from_oracle(P0, "Servo maker", SERVO_MAKER)
            .id();
        scenario.add_artifact_from_oracle(P0, "Synthetic", MAY_GAIN_ONE);
        scenario.with_mana_pool(
            P0,
            (0..3)
                .map(|i| ManaUnit::new(ManaType::Colorless, ObjectId(9_900 + i), false, Vec::new()))
                .collect(),
        );
        let mut runner = scenario.build();
        runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
        if with_basalt_first {
            let mana_idx = mana_ability_index(runner.state(), basalt).expect("mana ability");
            runner
                .act(GameAction::ActivateAbility {
                    source_id: basalt,
                    ability_index: mana_idx,
                })
                .expect("tap Basalt");
        }
        let expected_len = if with_basalt_first { 2 } else { 1 };
        let maker_idx = untap_ability_index(runner.state(), maker).expect("the token ability");
        runner
            .act(GameAction::ActivateAbility {
                source_id: maker,
                ability_index: maker_idx,
            })
            .expect("make a Servo");
        let mut asked = false;
        for _ in 0..30 {
            match &runner.state().waiting_for {
                WaitingFor::OptionalEffectChoice {
                    may_trigger_key, ..
                } => {
                    assert!(may_trigger_key.is_some());
                    assert_eq!(
                        runner.state().last_loop_action_sequence.len(),
                        expected_len,
                        "reach guard: the period at the prompt"
                    );
                    asked = true;
                    runner
                        .act(GameAction::DecideOptionalEffect { accept: true })
                        .expect("accept");
                    break;
                }
                WaitingFor::Priority { .. } => {
                    runner.act(GameAction::PassPriority).expect("pass");
                }
                other => panic!("unexpected prompt: {other:?}"),
            }
        }
        assert!(asked, "reach guard: the synthetic asked");
        let seq = &runner.state().last_loop_action_sequence;
        assert_eq!(seq.len(), expected_len, "the period is kept");
        if with_basalt_first {
            assert!(seq[0].pins.is_empty());
            let pins: Vec<MayChoiceOption> =
                may_pins(&runner, 1).into_iter().map(|p| p.1).collect();
            assert_eq!(pins, vec![MayChoiceOption::Take], "pinned one step later");
        } else {
            assert!(seq[0].pins.is_empty(), "no pin on the period's first step");
        }
    }
}

/// CR 603.5 + CR 732.2a: an OPPONENT's "may" is never a loop choice of the proposer: it is not
/// pinned and the period is not offered (the drive meets that prompt unpinned and aborts). The
/// paired positive is the same shape with the proposer's own "may": pinned and OFFERED.
#[test]
fn mana_engine_opponent_may_is_not_pinned_or_offered() {
    let mut rig = lap_rig(LapBoard {
        power: true,
        opponent_synthetic: &[OPPONENT_MAY_GAIN_ONE],
        ..LapBoard::BARE
    });
    let lap = human_lap(&mut rig, &[MayAnswer::Take], false);
    assert_eq!(lap.prompts.len(), 1, "reach guard: the opponent was asked");
    assert_eq!(lap.prompts[0].player, P1);
    assert!(lap.prompts[0].keyed);
    assert_eq!(
        lap.prompts[0].sequence_len_before, 2,
        "reach guard: asked during the proposer's untap step"
    );
    assert_eq!(
        life(&rig.runner, P1),
        21,
        "reach guard: the answer resolved"
    );
    assert_eq!(
        recorded_steps(&rig.runner),
        vec![(rig.mana_idx, 0), (rig.untap_idx, 0)]
    );
    assert_eq!(pins_per_step(&rig.runner), vec![0, 0], "never pinned");
    assert!(!offered(&rig.runner));

    let mut own = lap_rig(LapBoard {
        power: true,
        synthetic: &[MAY_GAIN_ONE],
        ..LapBoard::BARE
    });
    let lap = human_lap(&mut own, &[MayAnswer::Take], false);
    assert_eq!(lap.prompts.len(), 1);
    assert_eq!(lap.prompts[0].player, P0);
    assert_eq!(pins_per_step(&own.runner), vec![0, 1]);
    assert!(offered(&own.runner), "the proposer's own may is replayed");
}
