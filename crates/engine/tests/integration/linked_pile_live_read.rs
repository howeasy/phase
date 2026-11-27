//! The linked pile ("cards exiled with ~") is read when the effect is applied
//! (CR 608.2h), by the exact source incarnation only (CR 400.7), and "other"
//! after an exile in the same chain still excludes the card just exiled
//! (CR 608.2c).
//!
//! Printed cards, verbatim Oracle text (card-data.json of the base export):
//!   Mysterious Limousine {3}{W}{W}, Artifact - Vehicle 4/4: "Whenever this Vehicle
//!   enters or attacks, exile up to one other target creature until this Vehicle
//!   leaves the battlefield. If a creature is put into exile this way, return each
//!   other card exiled with this Vehicle to the battlefield under its owner's
//!   control.\nCrew 2"
//!   Mimic Vat {3}, Artifact: "Imprint - Whenever a nontoken creature dies, you may
//!   exile that card. If you do, return each other card exiled with this artifact to
//!   its owner's graveyard.\n{3}, {T}: Create a token that's a copy of a card exiled
//!   with this artifact. It gains haste. Exile it at the beginning of the next end
//!   step."
//!   Panharmonicon {4}, Artifact: "If an artifact or creature entering causes a
//!   triggered ability of a permanent you control to trigger, that ability triggers
//!   an additional time."
//!   Bishop of Binding {3}{W}, Creature - Vampire Cleric 1/1: "When this creature
//!   enters, exile target creature an opponent controls until this creature leaves
//!   the battlefield.\nWhenever this creature attacks, target Vampire gets +X/+X
//!   until end of turn, where X is the power of the exiled card."
//!   Cloudshift {W}, Instant: "Exile target creature you control, then return that
//!   card to the battlefield under your control."
//!   Wrath of God {2}{W}{W}, Sorcery: "Destroy all creatures. They can't be
//!   regenerated."
//!   Murder {1}{B}{B}, Instant: "Destroy target creature."

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{ExileLinkKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaColor};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MYSTERIOUS_LIMOUSINE: &str = "Whenever this Vehicle enters or attacks, exile up to one other target creature until this Vehicle leaves the battlefield. If a creature is put into exile this way, return each other card exiled with this Vehicle to the battlefield under its owner's control.\nCrew 2";
const MIMIC_VAT: &str = "Imprint \u{2014} Whenever a nontoken creature dies, you may exile that card. If you do, return each other card exiled with this artifact to its owner's graveyard.\n{3}, {T}: Create a token that's a copy of a card exiled with this artifact. It gains haste. Exile it at the beginning of the next end step.";
const PANHARMONICON: &str = "If an artifact or creature entering causes a triggered ability of a permanent you control to trigger, that ability triggers an additional time.";
const BISHOP_OF_BINDING: &str = "When this creature enters, exile target creature an opponent controls until this creature leaves the battlefield.\nWhenever this creature attacks, target Vampire gets +X/+X until end of turn, where X is the power of the exiled card.";
const CLOUDSHIFT: &str = "Exile target creature you control, then return that card to the battlefield under your control.";
const WRATH_OF_GOD: &str = "Destroy all creatures. They can't be regenerated.";

/// Answer each pending trigger target prompt in order from `targets`, stopping
/// at the first non-prompt state. Returns how many prompts were answered.
fn answer_trigger_targets(runner: &mut GameRunner, targets: &[ObjectId]) -> usize {
    let mut answered = 0;
    while let WaitingFor::TriggerTargetSelection { .. } = runner.state().waiting_for {
        let target = *targets
            .get(answered)
            .expect("a trigger target prompt with no scripted answer");
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .expect("trigger target choice");
        answered += 1;
    }
    answered
}

/// Pass priority until the stack is empty, ordering simultaneous triggers as
/// listed and accepting every optional effect. Returns how many optional
/// effects were accepted. Any other prompt fails the test by name.
fn drive_accepting_optionals(runner: &mut GameRunner) -> usize {
    let mut accepted = 0;
    for _ in 0..80 {
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                accepted += 1;
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return accepted,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("the stack resolves");
    }
    panic!("the stack never emptied");
}

/// CR 603.2d + CR 607.2a + CR 608.2c + CR 608.2h: Panharmonicon doubles the
/// Limousine's enters trigger; each instance chooses its own target. The first
/// resolving instance exiles its target (the pile is read live, so that card is
/// "exiled with" the Vehicle for the second instance); the second exiles its own
/// target and returns the card the FIRST exiled, but not its own ("other").
///
/// Mutants (each must turn this red):
///   - the trigger-bound pile reads only the latch (drop the live leg in
///     `players::pile_entries`): the first instance's card is invisible to the
///     second instance and stays exiled, so both targets end in exile;
///   - the "other" exclusion removed (`exclude_this_way_cards_from_other_linked_pile`
///     not applied): the first instance returns its own target, so after the first
///     resolution the reach guard below fails.
#[test]
fn doubled_mysterious_limousine_returns_the_first_instances_card_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first_target = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let second_target = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    scenario
        .add_artifact_from_oracle(P0, "Panharmonicon", PANHARMONICON)
        .with_mana_cost(parse_mtgjson_mana_cost("{4}"));
    let limo = scenario
        .add_artifact_to_hand_from_oracle(P0, "Mysterious Limousine", MYSTERIOUS_LIMOUSINE)
        .with_subtypes(vec!["Vehicle"])
        .with_mana_cost(parse_mtgjson_mana_cost("{3}{W}{W}"))
        .id();
    for _ in 0..5 {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    let mut runner = scenario.build();

    runner.cast(limo).commit();
    for _ in 0..10 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            || runner.state().objects[&limo].zone == Zone::Battlefield
        {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    assert_eq!(runner.state().objects[&limo].zone, Zone::Battlefield);
    // Two identical enters triggers are ordered first, then each picks its target.
    if let WaitingFor::OrderTriggers { triggers, .. } = runner.state().waiting_for.clone() {
        runner
            .act(GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            })
            .expect("order the doubled triggers");
    }
    // Each instance chooses its own up-to-one target as it is put on the stack.
    assert_eq!(
        answer_trigger_targets(&mut runner, &[first_target, second_target]),
        2,
        "Panharmonicon must put a second enters trigger on the stack"
    );
    assert_eq!(runner.state().stack.len(), 2, "reach: two instances");
    assert!(runner
        .state()
        .stack
        .iter()
        .all(|entry| entry.source_id == limo));

    // The top instance resolves: it exiles its own target, which stays exiled
    // (its "other" excludes the card it just exiled).
    runner.resolve_top();
    let (exiled_first, kept) = match (
        runner.state().objects[&first_target].zone,
        runner.state().objects[&second_target].zone,
    ) {
        (Zone::Exile, Zone::Battlefield) => (first_target, second_target),
        (Zone::Battlefield, Zone::Exile) => (second_target, first_target),
        zones => panic!("exactly one target is exiled by the first instance: {zones:?}"),
    };
    assert_eq!(runner.state().stack.len(), 1, "reach: one instance left");
    assert!(
        runner.state().exile_links.iter().any(|link| {
            link.exiled_id == exiled_first
                && link.source_id == limo
                && matches!(link.kind, ExileLinkKind::UntilSourceLeaves { .. })
        }),
        "reach: the first instance's card is exiled with the Limousine"
    );

    // The second instance exiles its own target and returns the first card.
    runner.resolve_top();
    assert!(runner.state().stack.is_empty());
    let state = runner.state();
    assert_eq!(
        state.objects[&kept].zone,
        Zone::Exile,
        "CR 608.2c: the card this instance just exiled is not returned"
    );
    assert_eq!(
        state.objects[&exiled_first].zone,
        Zone::Battlefield,
        "CR 608.2h + CR 603.2d: the card the first instance exiled is exiled with \
         the Limousine when the second instance applies its effect, so it returns"
    );
    assert_eq!(state.objects[&exiled_first].controller, P1);
}

/// Mimic Vat on P0's battlefield, two P1 creatures, and P0 holding Wrath of God
/// with four white lands to cast it.
struct VatBoard {
    runner: GameRunner,
    vat: ObjectId,
    victims: [ObjectId; 2],
}

fn vat_board_with_wrath() -> (VatBoard, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vat = scenario
        .add_artifact_from_oracle(P0, "Mimic Vat", MIMIC_VAT)
        .with_mana_cost(parse_mtgjson_mana_cost("{3}"))
        .id();
    let victims = [
        scenario.add_creature(P1, "Grizzly Bears", 2, 2).id(),
        scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id(),
    ];
    let wrath = scenario
        .add_spell_to_hand_from_oracle(P0, "Wrath of God", false, WRATH_OF_GOD)
        .with_mana_cost(parse_mtgjson_mana_cost("{2}{W}{W}"))
        .id();
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    (
        VatBoard {
            runner: scenario.build(),
            vat,
            victims,
        },
        wrath,
    )
}

/// CR 603.2d + CR 608.2h + CR 607.2a: two nontoken creatures die at once, so
/// Mimic Vat's imprint triggers twice. The ruling: "You'll wind up with at most
/// one of those cards exiled, and the rest will be in the appropriate
/// graveyard." The second instance reads the pile live (it holds the first
/// instance's card) and excludes the card it just exiled ("other").
///
/// Mutants: latch-only pile (drop the live leg in `players::pile_entries`) leaves
/// BOTH cards exiled; removing the `Not { LastZoneChanged }` leg of "other"
/// returns each instance's own card, so NEITHER stays exiled.
#[test]
fn mimic_vat_simultaneous_deaths_leave_exactly_one_card_exiled() {
    let (mut board, wrath) = vat_board_with_wrath();
    assert_eq!(
        board.runner.state().objects[&board.vat].zone,
        Zone::Battlefield,
        "reach: Mimic Vat is on the battlefield"
    );
    board.runner.cast(wrath).commit();
    let accepted = drive_accepting_optionals(&mut board.runner);
    assert_eq!(
        accepted, 2,
        "reach: both imprint triggers offered their exile"
    );

    let zones = board
        .victims
        .map(|card| board.runner.state().objects[&card].zone);
    let exiled = zones.iter().filter(|zone| **zone == Zone::Exile).count();
    let in_graveyard = zones
        .iter()
        .filter(|zone| **zone == Zone::Graveyard)
        .count();
    assert_eq!(
        (exiled, in_graveyard),
        (1, 1),
        "exactly one imprinted card stays exiled, the other is in its owner's graveyard: {zones:?}"
    );
    for card in board.victims {
        assert_eq!(board.runner.state().objects[&card].owner, P1);
    }
}

/// CR 400.7 + CR 607.2a + CR 608.2h (Bishop of Binding): the attack trigger of
/// the FIRST incarnation is on the stack; Bishop is blinked with Cloudshift (its
/// first exiled card returns as Bishop leaves), the new incarnation's enters
/// trigger exiles another creature under ITS link, and that link resolves BEFORE
/// the old trigger. The old trigger's X is "the power of the exiled card" read by
/// the old incarnation, which exiled nothing that is still exiled: X is 0 (CR 400.7,
/// CR 607.2a, CR 608.2h). The successor's link must not be read by the old
/// incarnation's trigger.
///
/// Positive control (reads X from the pile at all):
/// `bishop_of_binding_where_x_exiled_card_power::bishop_of_binding_pumps_target_vampire_by_exiled_card_power`
/// (6/6 with a 4/4 exiled; A is read through the latch there). That the live leg
/// admits a link of the live bound incarnation is controlled by
/// `doubled_mysterious_limousine_returns_the_first_instances_card_only` and the
/// unit tests in `players.rs`, not by this row.
///
/// Mutant: remove BOTH stale-reader guards of the live leg in
/// `players::pile_entries` (the `source.identity.is_current(state)` gate and the
/// `snap.supplier == reader` filter). The incarnation gate alone is redundant with
/// the pair filter on this board (the successor's link was made by the successor's
/// ability, so its pair names the new incarnation); the per-kind unit test
/// `a_stale_reader_never_reads_a_successor_incarnations_link_of_any_kind` fixes
/// the pair so that only the incarnation gate refuses, and fails on its removal.
/// With both removed the old trigger reads B (power 3) and the Vampire is 5/5.
#[test]
fn bishop_of_binding_old_attack_trigger_does_not_read_the_successors_exiled_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bishop = scenario
        .add_creature_to_hand_from_oracle(P0, "Bishop of Binding", 1, 1, BISHOP_OF_BINDING)
        .with_subtypes(vec!["Vampire", "Cleric"])
        .with_mana_cost(parse_mtgjson_mana_cost("{3}{W}"))
        .id();
    // The pump recipient: a second Vampire P0 controls (the trigger's target).
    let vampire = scenario
        .add_creature(P0, "Vampire Ally", 2, 2)
        .with_subtypes(vec!["Vampire"])
        .id();
    // A is exiled by the first incarnation's enters trigger; B by the second's.
    let card_a = scenario.add_creature(P1, "Exiled Brute", 4, 4).id();
    let card_b = scenario.add_creature(P1, "Second Brute", 3, 3).id();
    scenario.add_creature(P1, "Idle Bystander", 1, 1);
    let cloudshift = scenario
        .add_spell_to_hand_from_oracle(P0, "Cloudshift", true, CLOUDSHIFT)
        .with_mana_cost(parse_mtgjson_mana_cost("{W}"))
        .id();
    for _ in 0..5 {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    let mut runner = scenario.build();

    // Incarnation 1: its enters trigger exiles A under an `UntilSourceLeaves` link.
    runner.cast(bishop).target_object(card_a).resolve();
    assert_eq!(runner.state().objects[&card_a].zone, Zone::Exile);
    runner
        .state_mut()
        .objects
        .get_mut(&bishop)
        .unwrap()
        .summoning_sick = false;

    // Attack: the attack trigger (bound to this incarnation) targets the Vampire.
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(bishop, AttackTarget::Player(P1))])
        .expect("declare Bishop as an attacker");
    answer_trigger_targets(&mut runner, &[vampire]);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach: the attack trigger waits"
    );
    let attack_incarnation = {
        let entry = runner.state().stack.back().expect("the attack trigger");
        assert_eq!(entry.source_id, bishop);
        entry
            .ability()
            .and_then(|ability| ability.trigger_source.as_ref())
            .expect("a placed trigger carries its source context")
            .identity
            .reference
            .incarnation
    };

    // In response: Cloudshift on Bishop. A returns as Bishop leaves; Bishop
    // returns as a new incarnation whose enters trigger exiles B.
    runner.cast(cloudshift).target_object(bishop).commit();
    runner.resolve_top();
    assert_eq!(
        runner.state().objects[&card_a].zone,
        Zone::Battlefield,
        "reach: A returned when the first incarnation left"
    );
    assert_eq!(
        answer_trigger_targets(&mut runner, &[card_b]),
        1,
        "reach: the new incarnation's enters trigger asked for its target"
    );
    runner.resolve_top();

    // Reach guard, before the old trigger resolves.
    {
        let state = runner.state();
        assert_eq!(state.stack.len(), 1, "only the old attack trigger is left");
        let entry = state.stack.back().unwrap();
        assert_eq!(entry.source_id, bishop);
        let ability = entry.ability().expect("the old attack trigger");
        let bound = ability
            .trigger_source
            .as_ref()
            .unwrap()
            .identity
            .reference
            .incarnation;
        assert_eq!(
            bound, attack_incarnation,
            "still the attack-time incarnation"
        );
        assert_ne!(
            state.objects[&bishop].incarnation, bound,
            "Bishop is a new incarnation: the old trigger is a stale reader"
        );
        assert!(
            ability.targets.contains(&TargetRef::Object(vampire)),
            "the old trigger still targets the Vampire: {:?}",
            ability.targets
        );
        assert_eq!(state.objects[&card_b].zone, Zone::Exile);
        assert!(
            state.exile_links.iter().any(|link| link.exiled_id == card_b
                && link.source_id == bishop
                && matches!(link.kind, ExileLinkKind::UntilSourceLeaves { .. })),
            "B is exiled with Bishop (the new incarnation): {:?}",
            state.exile_links
        );
    }

    // The old trigger resolves.
    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("empty blocks");
            }
            other => panic!("unexpected prompt resolving the old trigger: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "reach: the old trigger resolved"
    );
    let pumped = &runner.state().objects[&vampire];
    assert_eq!(
        (pumped.power, pumped.toughness),
        (Some(2), Some(2)),
        "CR 400.7 + CR 608.2h: the old incarnation exiled no card that is still \
         exiled, so X is 0; a 5/5 means it read the successor's exiled card"
    );
    assert_eq!(runner.state().objects[&card_b].zone, Zone::Exile);
}
