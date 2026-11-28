//! `LingeringPermission:shared_budget_pile_election` on the printed faces.
//!
//! CR 601.2b + CR 118.9a: a face that writes a shared-budget free cast over its own exile pile
//! AND a second cast/play route over exiled cards leaves the player a choice between two methods
//! of casting, which `GameAction::CastSpell` cannot express. Court of Locthwain is the printed
//! example ("You may play that card for as long as it remains exiled, and mana of any type can be
//! spent to cast it. If you're the monarch, until end of turn, you may cast a spell from among
//! cards exiled with this enchantment without paying its mana cost"); it is reported unsupported.
//! Idol of Endurance and Serpent's Soul-Jar have one route over the pile and stay gap-free for
//! this reason. The remaining cards write no cast route at all, so they carry no election gap —
//! a no-election result, not a support claim: Assimilation Assessor is withdrawn and reported
//! unsupported by `UNTARGETED_OPPONENT_PILE_ASSIMILATE_GAP`.

use engine::database::CardDatabase;
use engine::game::coverage::{card_face_coverage, card_face_gaps, SHARED_BUDGET_PILE_ELECTION_GAP};
use engine::types::ability::{AbilityDefinition, CastFromZoneDriver, Effect, LingeringCastBudget};
use engine::types::card::CardFace;

fn face(name: &str) -> CardFace {
    CardDatabase::from_integration_fixture_cached()
        .expect("integration fixture loads")
        .get_face_by_name(name)
        .unwrap_or_else(|| panic!("{name} is in the integration fixture"))
        .clone()
}

fn chain_effects<'a>(def: &'a AbilityDefinition, out: &mut Vec<&'a Effect>) {
    out.push(&def.effect);
    if let Some(sub) = &def.sub_ability {
        chain_effects(sub, out);
    }
}

/// Every effect reachable through the face's abilities and trigger bodies by sub-ability chain.
fn face_effects(face: &CardFace) -> Vec<&Effect> {
    let mut out = Vec::new();
    for def in face
        .abilities
        .iter()
        .chain(face.triggers.iter().filter_map(|t| t.execute.as_deref()))
    {
        chain_effects(def, &mut out);
    }
    out
}

/// Reach guard: the face writes a shared-budget cast over its own exile pile.
fn has_shared_budget_pile_cast(face: &CardFace) -> bool {
    face_effects(face).into_iter().any(|effect| {
        matches!(
            effect,
            Effect::CastFromZone {
                target,
                driver: CastFromZoneDriver::LingeringPermission {
                    budget: LingeringCastBudget::SharedSingleCast
                },
                ..
            } if target.references_exiled_by_source()
        )
    })
}

fn has_play_grant(face: &CardFace) -> bool {
    face_effects(face)
        .into_iter()
        .any(|effect| matches!(effect, Effect::GrantCastingPermission { .. }))
}

fn carries_gap(face: &CardFace) -> bool {
    card_face_gaps(face)
        .iter()
        .any(|gap| gap == SHARED_BUDGET_PILE_ELECTION_GAP)
}

#[test]
fn court_of_locthwain_is_reported_unsupported_by_the_typed_election_gap() {
    let court = face("Court of Locthwain");
    assert!(has_shared_budget_pile_cast(&court), "reach: shared cast");
    assert!(has_play_grant(&court), "reach: ordinary play route");
    assert!(carries_gap(&court));

    let coverage = card_face_coverage(&court);
    assert!(coverage
        .gap_handlers()
        .iter()
        .any(|gap| gap == SHARED_BUDGET_PILE_ELECTION_GAP));
    assert!(!coverage.is_supported());

    // The typed gap does not lean on the confirmed-misparse marker.
    let mut unmarked = court.clone();
    unmarked.metadata.confirmed_misparse = None;
    assert!(carries_gap(&unmarked));
}

#[test]
fn single_route_shared_budget_faces_do_not_carry_the_election_gap() {
    for name in ["Idol of Endurance", "Serpent's Soul-Jar"] {
        let printed = face(name);
        assert!(
            has_shared_budget_pile_cast(&printed),
            "{name}: reach, shared cast over the pile"
        );
        assert!(!has_play_grant(&printed), "{name}: no second route");
        assert!(!carries_gap(&printed), "{name}");
    }
}

/// These faces write no cast route over exiled cards, so the election gap cannot apply. Absence
/// of this gap is not a support claim: Assimilation Assessor is reported unsupported by
/// `UNTARGETED_OPPONENT_PILE_ASSIMILATE_GAP`, and its presence here only pins that it does not
/// also write two cast routes.
#[test]
fn claimed_pile_readers_write_no_cast_route_and_carry_no_election_gap() {
    for name in [
        "Assimilation Assessor",
        "Borg Cube",
        "Duplicity",
        "Mimic Vat",
        "Mirror of Life Trapping",
        "Moonring Mirror",
        "Mysterious Limousine",
    ] {
        let printed = face(name);
        assert!(!has_shared_budget_pile_cast(&printed), "{name}");
        assert!(!has_play_grant(&printed), "{name}");
        assert!(!carries_gap(&printed), "{name}");
    }
}
