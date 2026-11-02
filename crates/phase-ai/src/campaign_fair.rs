//! Decision-local open-decklist sampling for native campaigns.
//!
//! The authoritative state is used only by the knowledge materializer and the
//! final legality check. No chooser, specialist, cache or RNG is initialized
//! until materialization has completed. This is not the ordinary K-sample
//! determinizer and it never retries a rejected action against the real world.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use engine::ai_support::AiDecisionContract;
use engine::database::CardDatabase;
use engine::game::derived::sync_continuous_reveals;
use engine::game::game_object::GameObject;
use engine::game::printed_cards::{
    apply_back_face_to_object, apply_card_face_to_object, back_face_for_card_face,
    snapshot_object_face,
};
use engine::game::visibility::{
    filter_state_for_viewer, known_object_ids_for_simulation, scrub_fair_simulation,
};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::game_state::{GameRng, GameState};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use rand::{seq::SliceRandom, SeedableRng};
use rand_chacha::ChaCha20Rng;
use sha2::{Digest, Sha256};

use crate::config::AiConfig;
use crate::search::choose_action_with_issued_contract;
use crate::session::AiSession;

/// A failure in the campaign information protocol, not an engine-rules defect.
/// The real driver separately reports an apply/contract violation as
/// `illegal_action/contract_violation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FairDecisionError {
    InvalidActor,
    DecisionOrdinalExhausted,
    MissingDeckPool {
        player: PlayerId,
    },
    HiddenPoolMismatch {
        player: PlayerId,
        slots: usize,
        cards: usize,
    },
    MissingCardDatabase {
        card: String,
    },
    UnsupportedOutsideGameChoice,
    UnknownToken {
        object: ObjectId,
    },
    MissingPublicProjection {
        object: ObjectId,
    },
    HiddenSlotCardinalityMismatch {
        player: PlayerId,
        zone: Zone,
    },
    RejectedSampleAction,
}

impl FairDecisionError {
    pub fn outcome(&self) -> &'static str {
        "fair_protocol"
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidActor => "actor_mismatch",
            Self::DecisionOrdinalExhausted => "decision_ordinal_exhausted",
            Self::MissingDeckPool { .. } => "missing_deck_pool",
            Self::HiddenPoolMismatch { .. } => "hidden_pool_mismatch",
            Self::MissingCardDatabase { .. } => "missing_card_database",
            Self::UnsupportedOutsideGameChoice => "unsupported_outside_game_choice",
            Self::UnknownToken { .. } => "unknown_token_origin",
            Self::MissingPublicProjection { .. } => "missing_public_projection",
            Self::HiddenSlotCardinalityMismatch { .. } => "hidden_slot_cardinality",
            Self::RejectedSampleAction => "rejected_sample_action",
        }
    }
}

impl fmt::Display for FairDecisionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}: {:?}", self.outcome(), self.code(), self)
    }
}
impl std::error::Error for FairDecisionError {}

pub struct FairDecision {
    pub action: Option<GameAction>,
    pub materialize_ns: u64,
    pub chooser_ns: u64,
}

/// Independent case seed and per-actor decision ordinals. No authoritative
/// state hash, RNG stream or per-game search session is retained here.
pub struct FairDecisionContext {
    sampling_seed: u64,
    ordinals: BTreeMap<PlayerId, u64>,
    card_database: Option<Arc<CardDatabase>>,
}

impl FairDecisionContext {
    pub fn new(sampling_seed: u64) -> Self {
        Self {
            sampling_seed,
            ordinals: BTreeMap::new(),
            card_database: None,
        }
    }

    pub fn with_card_database(mut self, database: Arc<CardDatabase>) -> Self {
        self.card_database = Some(database);
        self
    }

    pub fn choose(
        &mut self,
        state: &GameState,
        config: &AiConfig,
        semantic_owner: PlayerId,
        actor: PlayerId,
    ) -> Result<FairDecision, FairDecisionError> {
        let started = Instant::now();
        if !state.players.iter().any(|player| player.id == actor)
            || !state
                .players
                .iter()
                .any(|player| player.id == semantic_owner)
        {
            return Err(FairDecisionError::InvalidActor);
        }
        let ordinal = self.ordinals.entry(actor).or_default();
        let seed = fair_decision_seed(self.sampling_seed, actor, *ordinal, 0);
        *ordinal = ordinal
            .checked_add(1)
            .ok_or(FairDecisionError::DecisionOrdinalExhausted)?;
        let database = self
            .card_database
            .as_deref()
            .or_else(|| state.card_db.as_ref().map(|database| &**database));
        let synthetic = materialize_fair_state(state, actor, seed, database)?;
        let contract = AiDecisionContract::issue(&synthetic, semantic_owner);
        if contract.authorized_actor != actor {
            return Err(FairDecisionError::InvalidActor);
        }
        let session = AiSession::arc_from_game(&synthetic);
        let mut sampled_config = config.clone();
        // A root has already been sampled. The inner ensemble must neither
        // resample from state hashes nor retain an authoritative session.
        sampled_config.search.determinization_samples = 0;
        let mut chooser_rng = ChaCha20Rng::seed_from_u64(fair_decision_seed(
            self.sampling_seed,
            actor,
            *ordinal - 1,
            1,
        ));
        let materialize_ns = elapsed_ns(started);
        let chooser_started = Instant::now();
        let action = choose_action_with_issued_contract(
            &synthetic,
            &sampled_config,
            &mut chooser_rng,
            &session,
            &contract,
        );
        let chooser_ns = elapsed_ns(chooser_started);
        // This is a one-shot check, never a real-world candidate-ranking input.
        if let Some(action) = &action {
            validate_sample_action(state, semantic_owner, actor, action)?;
        }
        Ok(FairDecision {
            action,
            materialize_ns,
            chooser_ns,
        })
    }
}

/// Domain-separated, fixed-width little-endian seed protocol. The sample index
/// 0 denotes root materialization; 1 denotes chooser randomness.
pub fn fair_decision_seed(
    sampling_seed: u64,
    actor: PlayerId,
    ordinal: u64,
    sample_index: u32,
) -> u64 {
    let mut hash = Sha256::new();
    hash.update(b"phase/open-decklist-fair/v1\0");
    hash.update(sampling_seed.to_le_bytes());
    hash.update([actor.0]);
    hash.update(ordinal.to_le_bytes());
    hash.update(sample_index.to_le_bytes());
    let bytes = hash.finalize();
    u64::from_le_bytes(bytes[..8].try_into().expect("SHA256 has eight seed bytes"))
}

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Validates one sampled proposal using the engine's real contract. A sampled
/// private CardId is never translated by looking up the real hidden card. The
/// action's ObjectIds are engine-issued public handles/hidden choice slots;
/// identity-bearing casts of unknown sampled cards are rejected by membership.
pub fn validate_sample_action(
    state: &GameState,
    semantic_owner: PlayerId,
    actor: PlayerId,
    action: &GameAction,
) -> Result<(), FairDecisionError> {
    let contract = AiDecisionContract::issue(state, semantic_owner);
    if contract.authorized_actor != actor {
        return Err(FairDecisionError::InvalidActor);
    }
    if contract.permits(state, actor, action) {
        Ok(())
    } else {
        Err(FairDecisionError::RejectedSampleAction)
    }
}

fn face_key(face: &CardFace) -> (bool, &str) {
    face.scryfall_oracle_id
        .as_deref()
        .map_or((false, face.name.as_str()), |id| (true, id))
}

fn object_key(object: &GameObject) -> (bool, &str) {
    object
        .printed_ref
        .as_ref()
        .or(object.base_printed_ref.as_ref())
        .map_or((false, object.name.as_str()), |reference| {
            (true, reference.oracle_id.as_str())
        })
}

/// Constructs a fresh root without consulting any unknown card identity. Both
/// own-library order and opponents' hidden hand/library partitions are sampled.
/// Known identities are pinned using the engine visibility authority. Public
/// face-down bodies retain public status, while their private reverse face is
/// reconstructed from the sampled definition.
///
/// A missing multiset is an explicit protocol failure, never a partial sample
/// that leaves authoritative hidden identities in place. Standalone fixtures
/// without printed IDs can omit the database; production passes its pinned DB.
pub fn materialize_fair_state(
    state: &GameState,
    actor: PlayerId,
    seed: u64,
    database: Option<&CardDatabase>,
) -> Result<GameState, FairDecisionError> {
    if !state.players.iter().any(|player| player.id == actor) {
        return Err(FairDecisionError::InvalidActor);
    }
    let mut synthetic = state.clone();
    sync_continuous_reveals(&mut synthetic);
    let known = known_object_ids_for_simulation(&synthetic, actor);
    let unknown: HashSet<_> = synthetic
        .objects
        .keys()
        .copied()
        .filter(|id| !known.contains(id))
        .collect();
    // Wishboard choices are not open Commander registration data. Do not use a
    // private selected sideboard index to manufacture an apparently fair pool.
    if synthetic
        .outside_game_cards_brought_in
        .iter()
        .any(|entry| entry.player != actor)
    {
        return Err(FairDecisionError::UnsupportedOutsideGameChoice);
    }
    if let Some(object) = unknown
        .iter()
        .copied()
        .filter(|id| {
            synthetic
                .objects
                .get(id)
                .is_some_and(|object| object.is_token)
        })
        .min()
    {
        return Err(FairDecisionError::UnknownToken { object });
    }
    let public_face_down = unknown
        .iter()
        .any(|id| {
            synthetic
                .objects
                .get(id)
                .is_some_and(|object| !matches!(object.zone, Zone::Hand | Zone::Library))
        })
        .then(|| filter_state_for_viewer(&synthetic, actor));
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    let mut replacements = Vec::with_capacity(unknown.len());
    let mut holders: Vec<_> = synthetic
        .players
        .iter()
        .map(|player| synthetic.zone_storage_seat(Zone::Library, player.id))
        .collect();
    holders.sort_unstable();
    holders.dedup();
    for holder in holders {
        let mut slots: Vec<_> = unknown
            .iter()
            .copied()
            .filter(|id| {
                synthetic.objects.get(id).is_some_and(|object| {
                    !object.is_token
                        && synthetic.zone_storage_seat(Zone::Library, object.owner) == holder
                })
            })
            .collect();
        slots.sort_unstable();
        if slots.is_empty() {
            continue;
        }
        let pool = synthetic
            .deck_pool_of(holder)
            .ok_or(FairDecisionError::MissingDeckPool { player: holder })?;
        let mut counts: BTreeMap<(bool, &str), (&CardFace, usize)> = BTreeMap::new();
        for entry in pool
            .current_main
            .iter()
            .chain(pool.current_commander.iter())
        {
            let item = counts
                .entry(face_key(&entry.card))
                .or_insert((&entry.card, 0));
            item.1 += entry.count as usize;
        }
        // A declared companion is public, even after it moves to a hidden zone.
        // Only add an actual in-game companion, not the pre-game offer.
        for companion in synthetic
            .players
            .iter()
            .filter(|player| synthetic.zone_storage_seat(Zone::Library, player.id) == holder)
            .filter_map(|player| player.companion.as_ref())
            .filter(|companion| companion.used)
        {
            let face = &companion.card.card;
            let item = counts.entry(face_key(face)).or_insert((face, 0));
            item.1 += 1;
        }
        for entry in synthetic
            .outside_game_cards_brought_in
            .iter()
            .filter(|entry| entry.player == holder)
        {
            if let Some(card) = pool.current_sideboard.get(entry.sideboard_index) {
                let item = counts
                    .entry(face_key(&card.card))
                    .or_insert((&card.card, 0));
                item.1 += entry.count as usize;
            }
        }
        for (id, object) in &synthetic.objects {
            if known.contains(id)
                && !object.is_token
                && synthetic.zone_storage_seat(Zone::Library, object.owner) == holder
            {
                if let Some((_, count)) = counts.get_mut(&object_key(object)) {
                    *count = count.saturating_sub(1);
                }
            }
        }
        let mut cards = Vec::with_capacity(slots.len());
        for (_, (face, count)) in counts {
            cards.extend(std::iter::repeat_n(face, count));
        }
        if cards.len() != slots.len() {
            return Err(FairDecisionError::HiddenPoolMismatch {
                player: holder,
                slots: slots.len(),
                cards: cards.len(),
            });
        }
        cards.shuffle(&mut rng);
        for (id, face) in slots.into_iter().zip(cards) {
            if face.scryfall_oracle_id.is_some()
                && !database.is_some_and(|db| {
                    db.get_face_by_name(&face.name).is_some_and(|registered| {
                        registered.scryfall_oracle_id == face.scryfall_oracle_id
                    })
                })
            {
                return Err(FairDecisionError::MissingCardDatabase {
                    card: face.name.clone(),
                });
            }
            let old = &synthetic.objects[&id];
            let mut object = GameObject::new(
                id,
                CardId((1u64 << 63) | id.0),
                old.owner,
                face.name.clone(),
                old.zone,
            );
            apply_card_face_to_object(&mut object, face);
            if let Some(engine::types::card::CardRules {
                layout: engine::types::card::CardLayout::Specialize(_, variants),
                ..
            }) = database.and_then(|db| db.get_by_name(&face.name))
            {
                object.specialize_faces = Some(
                    engine::game::specialize::specialize_faces_from_variants(variants),
                );
            }
            object.back_face = database.and_then(|db| back_face_for_card_face(db, face));
            object.controller = old.controller;
            object.base_controller = old.base_controller;
            object.timestamp = old.timestamp;
            object.incarnation = old.incarnation;
            if !matches!(old.zone, Zone::Hand | Zone::Library) {
                let back = snapshot_object_face(&object);
                let public = public_face_down
                    .as_ref()
                    .and_then(|projection| projection.objects.get(&id))
                    .ok_or(FairDecisionError::MissingPublicProjection { object: id })?;
                apply_back_face_to_object(&mut object, snapshot_object_face(public));
                object.back_face = Some(back);
                object.face_down = public.face_down;
                object.face_down_cause = public.face_down_cause;
                object.tapped = public.tapped;
                object.damage_marked = public.damage_marked;
                object.dealt_deathtouch_damage = public.dealt_deathtouch_damage;
                object.counters = public.counters.clone();
                object.attached_to = public.attached_to;
                object.attachments = public.attachments.clone();
                object.paired_with = public.paired_with;
                object.pair_controller = public.pair_controller;
                object.entered_battlefield_turn = public.entered_battlefield_turn;
                object.summoning_sick = public.summoning_sick;
                object.foretold = public.foretold;
            }
            replacements.push((id, object));
        }
    }
    for (id, object) in replacements {
        synthetic.objects.insert(id, object);
    }
    scrub_fair_simulation(&mut synthetic, actor, &unknown);
    // Hidden handles must not disclose a real permutation through iteration
    // order. Preserve positions of known cards, canonicalize only unknown slots.
    for player in &mut synthetic.players {
        let mut library: Vec<_> = player
            .library
            .iter()
            .copied()
            .filter(|id| unknown.contains(id))
            .collect();
        library.sort_unstable();
        let mut library = library.into_iter();
        for id in player.library.iter_mut() {
            if unknown.contains(id) {
                *id = library
                    .next()
                    .ok_or(FairDecisionError::HiddenSlotCardinalityMismatch {
                        player: player.id,
                        zone: Zone::Library,
                    })?;
            }
        }
        let mut hand: Vec<_> = player
            .hand
            .iter()
            .copied()
            .filter(|id| unknown.contains(id))
            .collect();
        hand.sort_unstable();
        let mut hand = hand.into_iter();
        for id in player.hand.iter_mut() {
            if unknown.contains(id) {
                *id = hand
                    .next()
                    .ok_or(FairDecisionError::HiddenSlotCardinalityMismatch {
                        player: player.id,
                        zone: Zone::Hand,
                    })?;
            }
        }
    }
    synthetic.rng_seed = seed;
    synthetic.rng_word_pos = 0;
    synthetic.rng = GameRng::seed_from_u64(seed);
    Ok(synthetic)
}
