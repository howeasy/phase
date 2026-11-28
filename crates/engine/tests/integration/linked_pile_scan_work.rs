//! Maintainer review item: "pile membership is evaluated per candidate and each evaluation scans
//! the flat link store". A scan over an exile population reads the link store once, whatever the
//! candidate count (CR 607.2a: "cards exiled with ~" is one pile per source); a candidate outside
//! exile never reads it (CR 400.7: a card not in exile cannot be an exiled-with card); and every
//! answer is the one the direct per-candidate probe gives.
//!
//! The instrument is `filter::link_store_work().scans`, counted at `links_for_reader`, the one
//! funnel behind both the live-link read (no trigger source) and the trigger-bound pile read.
//!
//! Printed cards, filters read from the integration fixture's parsed faces, verbatim Oracle text:
//!   Idol of Endurance {2}{W}, Artifact: "When this artifact enters, exile all creature cards with
//!   mana value 3 or less from your graveyard until this artifact leaves the battlefield.\n{1}{W},
//!   {T}: Until end of turn, you may cast a creature spell from among cards exiled with this
//!   artifact without paying its mana cost."
//!   Assimilation Assessor {4}{B}, Artifact Creature - Borg 5/5 (fixture a.jsonl): "When this creature
//!   enters, exile target opponent's graveyard.\nWhenever this creature attacks, assimilate a
//!   creature card exiled with it. (Put it onto the battlefield under your control with a +1/+1
//!   counter. It's a Borg artifact creature and loses all other creature types.)"
//!   Creeping Inn, Artifact Creature - Horror Construct (fixture c.jsonl): "Whenever this creature attacks, you may exile
//!   a creature card from your graveyard. If you do, each opponent loses X life and you gain X
//!   life, where X is the number of creature cards exiled with this creature.\n{4}: This creature
//!   phases out."
//!   Koh, the Face Stealer, Creature (fixture k.jsonl): "When Koh enters, exile up to
//!   one other target creature.\nWhenever another nontoken creature dies, you may exile it.\nPay 1
//!   life: Choose a creature card exiled with Koh.\nKoh has all activated and triggered abilities
//!   of the last chosen card."
//!   Chaos Wand, Artifact (fixture c.jsonl): "{4}, {T}: Target opponent exiles cards from the
//!   top of their library until they exile an instant or sorcery card. You may cast that card
//!   without paying its mana cost. Then put the exiled cards that weren't cast this way on the
//!   bottom of that library in a random order."
//!   Mirror of Life Trapping, Artifact (fixture m.jsonl): "Whenever a creature enters,
//!   if it was cast, exile it, then return all other permanent cards exiled with this artifact to
//!   the battlefield under their owners' control."
//!
//! Assimilation Assessor is reported unsupported
//! (`UNTARGETED_OPPONENT_PILE_ASSIMILATE_GAP`); its text is used here only as
//! the vehicle for the linked-pile scan work measurement.

use std::sync::OnceLock;

use engine::database::CardDatabase;
use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::filter::{
    link_store_work, matches_target_filter, reset_link_store_work, FilterContext,
};
use engine::game::layers::evaluate_layers;
use engine::game::quantity::resolve_quantity;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::targeting::{find_legal_targets, find_legal_targets_for_ability};
use engine::game::triggers::trigger_source_context_for_latch;
use engine::types::ability::{AbilityDefinition, AbilityKind};
use engine::types::ability::{
    ContinuousModification, Effect, FilterProp, ResolvedAbility, StaticDefinition, TargetFilter,
    TargetRef, TypedFilter,
};
use engine::types::game_state::{ExileLink, ExileLinkKind, GameState, LayersDirty, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::zones::Zone;

const IDOL_OF_ENDURANCE: &str = "When this artifact enters, exile all creature cards with mana \
     value 3 or less from your graveyard until this artifact leaves the battlefield.\n\
     {1}{W}, {T}: Until end of turn, you may cast a creature spell from among cards exiled with \
     this artifact without paying its mana cost.";

const ASSIMILATION_ASSESSOR: &str = "When this creature enters, exile target opponent's \
     graveyard.\nWhenever this creature attacks, assimilate a creature card exiled with it. \
     (Put it onto the battlefield under your control with a +1/+1 counter. It's a Borg artifact \
     creature and loses all other creature types.)";

fn card_db() -> &'static CardDatabase {
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    DB.get_or_init(|| CardDatabase::from_integration_fixture_cached().expect("fixture loads"))
}

/// The printed free-cast pool of Idol of Endurance: `And[Creature in Exile, ExiledBySource]`.
fn idol_cast_effect() -> Effect {
    let face = card_db()
        .get_face_by_name("Idol of Endurance")
        .expect("Idol of Endurance is in the fixture");
    face.abilities
        .iter()
        .map(|def| (*def.effect).clone())
        .find(|effect| matches!(effect, Effect::CastFromZone { .. }))
        .expect("Idol carries its CastFromZone")
}

fn idol_pile_filter() -> TargetFilter {
    match idol_cast_effect() {
        Effect::CastFromZone { target, .. } => target,
        other => panic!("unexpected effect {other:?}"),
    }
}

/// The printed assimilate pool of Assimilation Assessor: `And[Creature, ExiledBySource]`.
fn assessor_pile_filter() -> TargetFilter {
    let face = card_db()
        .get_face_by_name("Assimilation Assessor")
        .expect("Assimilation Assessor is in the fixture");
    face.triggers
        .iter()
        .filter_map(|trigger| trigger.execute.as_ref())
        .find_map(|def| match &*def.effect {
            Effect::ChangeZone { target, .. } => Some(target.clone()),
            _ => None,
        })
        .expect("Assessor's attack trigger moves a creature card exiled with it")
}

/// Idol of Endurance on the battlefield, `pile` creature cards exiled with it and `filler` more in
/// P0's exile that belong to no source. Returns the state, the Idol and the pile ids.
fn idol_world(filler: usize, pile: usize) -> (GameState, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    let idol = scenario
        .add_artifact_from_oracle(P0, "Idol of Endurance", IDOL_OF_ENDURANCE)
        .id();
    for n in 0..filler {
        scenario.add_creature_to_exile(P0, &format!("Filler {n}"), 1, 1);
    }
    let members: Vec<ObjectId> = (0..pile)
        .map(|n| {
            scenario
                .add_creature_to_exile(P0, &format!("Pile {n}"), 2, 2)
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    for exiled_id in &members {
        runner.state_mut().exile_links.push(ExileLink {
            exiled_id: *exiled_id,
            source_id: idol,
            kind: ExileLinkKind::TrackedBySource,
            supplier: None,
        });
    }
    (runner.state().clone(), idol, members)
}

fn object_ids(targets: &[TargetRef]) -> Vec<ObjectId> {
    let mut ids: Vec<ObjectId> = targets
        .iter()
        .filter_map(|target| match target {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();
    ids.sort();
    ids
}

fn sorted(mut ids: Vec<ObjectId>) -> Vec<ObjectId> {
    ids.sort();
    ids
}

/// Link-store scans one measured closure performs on this thread.
fn scans_of<R>(measured: impl FnOnce() -> R) -> (R, usize) {
    reset_link_store_work();
    let result = measured();
    (result, link_store_work().scans)
}

/// CR 607.2a + CR 400.7: the live-link reading (no trigger source) of Idol's pool over an exile
/// population reads the link store once, at 4 and at 40 non-pile exile cards alike.
#[test]
fn linked_pile_scan_work_live_reading_is_independent_of_candidate_count() {
    let filter = idol_pile_filter();
    let measure = |filler: usize| {
        let (state, idol, members) = idol_world(filler, 3);
        let (targets, scans) = scans_of(|| find_legal_targets(&state, &filter, P0, idol));
        assert_eq!(
            object_ids(&targets),
            sorted(members),
            "the offered pool is exactly the pile"
        );
        scans
    };
    let (small, large) = (measure(4), measure(40));
    // Reach guard: the instrument fires on the enumeration.
    assert!(small >= 1, "a pile scan reads the link store");
    assert_eq!(small, large, "scans do not grow with the candidate count");
    assert!(large <= 2, "one read per scan, not per candidate: {large}");
}

/// CR 603.10a + CR 607.2a + CR 400.7: the trigger-bound reading (the ability carries its latched
/// source) reads the link store once per scan too.
#[test]
fn linked_pile_scan_work_trigger_bound_reading_is_independent_of_candidate_count() {
    let filter = idol_pile_filter();
    let measure = |filler: usize| {
        let (state, idol, members) = idol_world(filler, 3);
        let mut ability = ResolvedAbility::new(idol_cast_effect(), vec![], idol, P0);
        ability.trigger_source = Some(trigger_source_context_for_latch(
            &state,
            &state.objects[&idol],
        ));
        let (targets, scans) =
            scans_of(|| find_legal_targets_for_ability(&state, &filter, &ability));
        assert_eq!(
            object_ids(&targets),
            sorted(members),
            "the offered pool is exactly the latched pile"
        );
        scans
    };
    let (small, large) = (measure(4), measure(40));
    assert_eq!(small, large, "scans do not grow with the candidate count");
    assert!(large <= 2, "one read per scan, not per candidate: {large}");
}

/// CR 400.7: a battlefield candidate is not a card exiled with anything, so the Assessor's pool
/// filter evaluated over 40 battlefield creatures never touches the link store; a pile of three
/// exile cards makes the same filter read it (positive control).
#[test]
fn linked_pile_scan_work_non_exile_candidates_read_no_link() {
    let filter = assessor_pile_filter();
    let mut scenario = GameScenario::new();
    let assessor = scenario
        .add_creature_from_oracle(P0, "Assimilation Assessor", 5, 5, ASSIMILATION_ASSESSOR)
        .as_artifact_creature()
        .with_subtypes(vec!["Borg"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 4,
        })
        .id();
    let battlefield: Vec<ObjectId> = (0..40)
        .map(|n| scenario.add_creature(P1, &format!("Bear {n}"), 2, 2).id())
        .collect();
    let pile: Vec<ObjectId> = (0..3)
        .map(|n| {
            scenario
                .add_creature_to_exile(P1, &format!("Exiled {n}"), 1, 1)
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    for exiled_id in &pile {
        runner.state_mut().exile_links.push(ExileLink {
            exiled_id: *exiled_id,
            source_id: assessor,
            kind: ExileLinkKind::TrackedBySource,
            supplier: None,
        });
    }
    let state = runner.state();
    let ctx = FilterContext::from_source(state, assessor);

    let (hits, scans) = scans_of(|| {
        battlefield
            .iter()
            .filter(|id| matches_target_filter(state, **id, &filter, &ctx))
            .count()
    });
    assert_eq!(
        hits, 0,
        "no battlefield creature is exiled with the Assessor"
    );
    assert_eq!(scans, 0, "a candidate outside exile reads no link");

    // Positive control: the pile members are exiled with it and do read the store.
    let (matched, scans) = scans_of(|| {
        pile.iter()
            .filter(|id| matches_target_filter(state, **id, &filter, &ctx))
            .count()
    });
    assert_eq!(matched, pile.len(), "every pile card matches the pool");
    assert!(scans >= 1, "the instrument fires on exile candidates");
}

// ---------------------------------------------------------------------------------------------
// Wired consumers: every scan below runs the printed card's own effect through the production
// resolver and reads the link store once whatever the number of unrelated exile cards.
// ---------------------------------------------------------------------------------------------

/// One source on the battlefield with three creature cards exiled with it, a rival source whose
/// own exiled card must never be counted as the first source's, and `filler` unrelated exile
/// cards.
struct PileWorld {
    runner: GameRunner,
    source: ObjectId,
    pile: Vec<ObjectId>,
    rival_card: ObjectId,
    filler: Vec<ObjectId>,
}

fn pile_world(filler: usize) -> PileWorld {
    let mut scenario = GameScenario::new();
    let source = scenario.add_creature(P0, "Pile Source", 2, 2).id();
    let rival = scenario.add_creature(P0, "Rival Source", 2, 2).id();
    let filler_ids: Vec<ObjectId> = (0..filler)
        .map(|n| {
            scenario
                .add_creature_to_exile(P1, &format!("Filler {n}"), 1, 1)
                .id()
        })
        .collect();
    let pile: Vec<ObjectId> = (0..3)
        .map(|n| {
            scenario
                .add_creature_to_exile(P1, &format!("Pile {n}"), 2, 2)
                .id()
        })
        .collect();
    let rival_card = scenario.add_creature_to_exile(P1, "Rival Card", 3, 3).id();
    let mut runner = scenario.build();
    for (exiled_id, source_id) in pile
        .iter()
        .map(|id| (*id, source))
        .chain([(rival_card, rival)])
    {
        runner.state_mut().exile_links.push(ExileLink {
            exiled_id,
            source_id,
            kind: ExileLinkKind::TrackedBySource,
            supplier: None,
        });
    }
    PileWorld {
        runner,
        source,
        pile,
        rival_card,
        filler: filler_ids,
    }
}

/// The first effect of `face`'s abilities, triggers' executes and their sub-ability chains for
/// which `pick` answers `true`, with its definition.
fn printed_definition(name: &str, pick: impl Fn(&Effect) -> bool) -> AbilityDefinition {
    let face = card_db()
        .get_face_by_name(name)
        .unwrap_or_else(|| panic!("{name} is in the fixture"));
    let roots = face
        .abilities
        .iter()
        .chain(face.triggers.iter().filter_map(|t| t.execute.as_deref()));
    for root in roots {
        let mut node = Some(root);
        while let Some(def) = node {
            if pick(&def.effect) {
                return def.clone();
            }
            node = def.sub_ability.as_deref();
        }
    }
    panic!("{name} carries no matching effect");
}

/// CR 607.2a: "the number of creature cards exiled with this creature" (Creeping Inn) counts
/// the pile with one link-store read at 4 and at 40 unrelated exile cards, and never counts
/// another source's exiled card.
#[test]
fn linked_pile_scan_work_object_count_over_a_pile() {
    let amount = match printed_definition("Creeping Inn", |e| matches!(e, Effect::LoseLife { .. }))
        .effect
        .as_ref()
    {
        Effect::LoseLife { amount, .. } => amount.clone(),
        other => panic!("unexpected effect {other:?}"),
    };
    let measure = |filler: usize| {
        let world = pile_world(filler);
        let (value, scans) =
            scans_of(|| resolve_quantity(world.runner.state(), &amount, P0, world.source));
        assert_eq!(value, 3, "exactly the three cards exiled with the source");
        scans
    };
    let (small, large) = (measure(4), measure(40));
    assert!(small >= 1, "the instrument fires on the count");
    assert_eq!(small, large, "scans do not grow with the candidate count");
}

/// CR 607.2a + CR 400.1: "Choose a creature card exiled with Koh" offers exactly the pile out
/// of the shared exile zone with one link-store read at 4 and at 40 unrelated exile cards.
#[test]
fn linked_pile_scan_work_choose_from_zone_offers_exactly_the_pile() {
    let def = printed_definition("Koh, the Face Stealer", |e| {
        matches!(e, Effect::ChooseFromZone { .. })
    });
    assert_eq!(def.kind, AbilityKind::Activated);
    let measure = |filler: usize| {
        let mut world = pile_world(filler);
        let ability = build_resolved_from_def(&def, world.source, P0);
        let mut events = Vec::new();
        let ((), scans) = scans_of(|| {
            resolve_ability_chain(world.runner.state_mut(), &ability, &mut events, 0)
                .expect("the choice parks")
        });
        match &world.runner.state().waiting_for {
            WaitingFor::ChooseFromZoneChoice { cards, .. } => {
                assert_eq!(
                    sorted(cards.clone()),
                    sorted(world.pile.clone()),
                    "the offered cards are exactly the pile"
                );
                assert!(!cards.contains(&world.rival_card));
            }
            other => panic!("expected the pile choice, got {other:?}"),
        }
        scans
    };
    let (small, large) = (measure(4), measure(40));
    assert!(small >= 1, "the instrument fires on the pool scan");
    assert_eq!(small, large, "scans do not grow with the candidate count");
}

/// CR 607.2a: "put the exiled cards ... on the bottom of that library" (Chaos Wand)
/// moves exactly the pile, with one link-store read at 4 and at 40 unrelated exile cards.
#[test]
fn linked_pile_scan_work_put_on_bottom_moves_exactly_the_pile() {
    let def = printed_definition("Chaos Wand", |e| {
        matches!(e, Effect::PutAtLibraryPosition { .. })
    });
    let measure = |filler: usize| {
        let mut world = pile_world(filler);
        let ability = build_resolved_from_def(&def, world.source, P0);
        let mut events = Vec::new();
        let ((), scans) = scans_of(|| {
            resolve_ability_chain(world.runner.state_mut(), &ability, &mut events, 0)
                .expect("the pile is put on the bottom")
        });
        let state = world.runner.state();
        for id in &world.pile {
            assert_eq!(
                state.objects[id].zone,
                Zone::Library,
                "{id:?} is in a library"
            );
        }
        for id in world.filler.iter().chain([&world.rival_card]) {
            assert_eq!(state.objects[id].zone, Zone::Exile, "{id:?} stays exiled");
        }
        scans
    };
    let (small, large) = (measure(4), measure(40));
    assert!(small >= 1, "the instrument fires on the exile scan");
    assert_eq!(small, large, "scans do not grow with the candidate count");
}

/// CR 607.2a + CR 400.7: "return all other permanent cards exiled with this artifact to the
/// battlefield" (Mirror of Life Trapping) mass-moves exactly the pile with one link-store read
/// at 4 and at 40 unrelated exile cards.
#[test]
fn linked_pile_scan_work_mass_return_moves_exactly_the_pile() {
    let def = printed_definition("Mirror of Life Trapping", |e| {
        matches!(e, Effect::ChangeZoneAll { .. })
    });
    let measure = |filler: usize| {
        let mut world = pile_world(filler);
        let ability = build_resolved_from_def(&def, world.source, P0);
        let mut events = Vec::new();
        let ((), scans) = scans_of(|| {
            resolve_ability_chain(world.runner.state_mut(), &ability, &mut events, 0)
                .expect("the pile returns")
        });
        let state = world.runner.state();
        for id in &world.pile {
            assert_eq!(
                state.objects[id].zone,
                Zone::Battlefield,
                "{id:?} returned to the battlefield"
            );
        }
        for id in world.filler.iter().chain([&world.rival_card]) {
            assert_eq!(state.objects[id].zone, Zone::Exile, "{id:?} stays exiled");
        }
        scans
    };
    let (small, large) = (measure(4), measure(40));
    assert!(small >= 1, "the instrument fires on the exile scan");
    assert_eq!(small, large, "scans do not grow with the candidate count");
}

/// CR 613.1f + CR 607.2a: SYNTHETIC (no printed card has a static whose `affected` filter names
/// "cards exiled with ~"; Koh's grant copies abilities instead): a static whose `affected` is
/// `And[Creature in exile, ExiledBySource]` grants flying to exactly the pile, and a layer pass
/// reads the link store once for it whatever the number of unrelated exile cards. A static whose
/// population is the battlefield alone reads none (a battlefield object is never exiled with
/// anything).
#[test]
fn linked_pile_scan_work_layer_gather_over_exile_reads_the_pile_once() {
    let pile_static = || {
        StaticDefinition::continuous()
            .affected(TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .properties(vec![FilterProp::InZone { zone: Zone::Exile }]),
                    ),
                    TargetFilter::ExiledBySource,
                ],
            })
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            }])
    };
    let measure = |filler: usize, definition: StaticDefinition| {
        let mut world = pile_world(filler);
        {
            let source = world
                .runner
                .state_mut()
                .objects
                .get_mut(&world.source)
                .unwrap();
            source.static_definitions = vec![definition].into();
        }
        let (granted, scans) = scans_of(|| {
            let state = world.runner.state_mut();
            state.layers_dirty = LayersDirty::Full;
            evaluate_layers(state);
        });
        let state = world.runner.state();
        let flying = |id: &ObjectId| state.objects[id].has_keyword(&Keyword::Flying);
        (
            granted,
            scans,
            world.pile.iter().all(flying),
            world.filler.iter().any(flying),
        )
    };
    let (_, small, pile_flies, filler_flies) = measure(4, pile_static());
    let (_, large, _, _) = measure(40, pile_static());
    assert!(
        pile_flies,
        "reach guard: the static grants flying to the pile"
    );
    assert!(!filler_flies, "no unrelated exile card is granted flying");
    assert!(small >= 1, "the instrument fires on the gather");
    assert_eq!(small, large, "scans do not grow with the exile population");

    // A battlefield-only static in the same state reads no link.
    let battlefield_only = StaticDefinition::continuous()
        .affected(TargetFilter::Typed(TypedFilter::creature()))
        .modifications(vec![ContinuousModification::AddKeyword {
            keyword: Keyword::Flying,
        }]);
    let (_, scans, _, _) = measure(40, battlefield_only);
    assert_eq!(scans, 0, "a battlefield population reads no link");
}
