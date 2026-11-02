//! Bounded native campaign protocol. Observations are display data, never saves.
//! Engine contracts and the existing typed replay action remain the authorities.
//! `reproduce.json` is the immutable case manifest; reproduction tooling stages
//! its pinned export/corpus beside it before invoking the same native command.
//! Segment indexes are immutable, monotonically numbered snapshots. Only sealed
//! segments have durable SHA256 receipts; collectors recover an unsealed prefix
//! after proving process exit. Native resource samples are pre-exit observations;
//! the worker publishes final cgroup accounting separately without rewriting logs.
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use engine::ai_support::AiDecisionContract;
use engine::database::CardDatabase;
use engine::game::deck_loading::{
    load_and_hydrate_decks, resolve_deck_list, DeckList, DeckPayload, PlayerDeckList,
};
use engine::game::derived_views::ClientGameStateRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, PersistedGameState, WaitingFor};
use engine::types::player::PlayerId;
use engine::types::replay::{
    RecordedAction, RecordedActionKind, ReplayHeader, REPLAY_FORMAT_VERSION,
};
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::auto_play::{
    run_driver_loop_observed, AiActionResult, AiActionsStop, DriverExit, DriverHooks,
};
use crate::campaign_fair::FairDecisionContext;
use crate::config::{create_config_for_players, AiConfig, AiDifficulty, Platform};
use crate::session::AiSession;

pub const SCHEMA_VERSION: u32 = 1;
pub const ACTION_CAP: usize = 200_000;
pub const OBSERVATION_LIMIT: usize = 8 << 20;
pub const JOURNAL_LIMIT: u64 = 2 << 30;
const TERMINAL_RESERVE: u64 = OBSERVATION_LIMIT as u64;
const SEGMENT_BYTES: u64 = 1 << 20;
const CHECKPOINT_ACTIONS: u64 = 128;

type Result<T, E = String> = std::result::Result<T, E>;

/// A transparent JSON value whose decoder rejects duplicate keys at every depth.
struct UniqueValue(Value);
impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = UniqueValue;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("unique-field JSON")
            }
            fn visit_bool<E: serde::de::Error>(
                self,
                v: bool,
            ) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Self::Value, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| UniqueValue(Value::Number(n)))
                    .ok_or_else(|| E::custom("nonfinite number"))
            }
            fn visit_str<E: serde::de::Error>(
                self,
                v: &str,
            ) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_string<E: serde::de::Error>(
                self,
                v: String,
            ) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(Value::Null))
            }
            fn visit_none<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(Value::Null))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(UniqueValue(v)) = a.next_element()? {
                    values.push(v);
                }
                Ok(UniqueValue(Value::Array(values)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some((k, UniqueValue(v))) = a.next_entry::<String, UniqueValue>()? {
                    if values.insert(k.clone(), v).is_some() {
                        return Err(serde::de::Error::custom(format!("duplicate field: {k}")));
                    }
                }
                Ok(UniqueValue(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

fn required_nullable<'de, D, T>(d: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(d)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InformationModel {
    OmniscientStress,
    OpenDecklistFairV1,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignManifest {
    pub schema_version: u32,
    pub game_key: String,
    pub epoch_id: String,
    pub seed: String,
    #[serde(deserialize_with = "required_nullable")]
    pub sampling_seed: Option<String>,
    pub track: String,
    #[serde(deserialize_with = "required_nullable")]
    pub phase: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub panel_id: Option<u64>,
    #[serde(deserialize_with = "required_nullable")]
    pub block_id: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub pair_id: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub role: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub anchor_tier: Option<String>,
    pub config_id: String,
    #[serde(deserialize_with = "required_nullable")]
    pub evaluated_seat: Option<u8>,
    pub information_model: InformationModel,
    pub fair_contract_version: u32,
    pub pod: [String; 4],
    /// Complete generated effective configs, not caller-selected arbitrary knobs.
    pub seats: [Value; 4],
    pub action_cap: usize,
    pub export_path: PathBuf,
    pub feed_path: PathBuf,
}

pub fn decimal_seed(value: &str) -> Result<u64> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|c| c.is_ascii_digit())
    {
        return Err("seed must be a canonical decimal-u64 string".into());
    }
    value.parse().map_err(|_| "seed exceeds u64".into())
}
fn digest_valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// Python depot.canonical-compatible JSON (including its float exponent spelling).
pub fn canonical_bytes(value: &Value) -> Result<Vec<u8>> {
    fn encode(v: &Value, out: &mut Vec<u8>) -> Result<()> {
        match v {
            Value::Object(map) => {
                out.push(b'{');
                let sorted: BTreeMap<_, _> = map.iter().collect();
                for (i, (key, value)) in sorted.into_iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    serde_json::to_writer(&mut *out, key).map_err(|e| e.to_string())?;
                    out.push(b':');
                    encode(value, out)?;
                }
                out.push(b'}');
            }
            Value::Array(values) => {
                out.push(b'[');
                for (i, value) in values.iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    encode(value, out)?;
                }
                out.push(b']');
            }
            Value::Number(n) if n.is_f64() => {
                let f = n.as_f64().ok_or("nonfinite number")?;
                let mut text = n.to_string();
                if f != 0.0 && (f.abs() < 0.0001 || f.abs() >= 1e16) {
                    let scientific = format!("{f:e}");
                    let (mantissa, exp) = scientific.split_once('e').ok_or("invalid exponent")?;
                    let exponent: i32 = exp.parse().map_err(|_| "invalid exponent")?;
                    text = format!("{mantissa}e{exponent:+03}");
                }
                out.extend_from_slice(text.as_bytes());
            }
            _ => serde_json::to_writer(&mut *out, v).map_err(|e| e.to_string())?,
        }
        Ok(())
    }
    let mut bytes = Vec::new();
    encode(value, &mut bytes)?;
    Ok(bytes)
}
pub fn canonical_hash(value: &Value) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(canonical_bytes(value)?)))
}

/// Full authoritative serialized state plus the live RNG position. Unlike the
/// display checkpoint hash this detects changes in private continuation state.
pub fn deterministic_state_hash(state: &GameState) -> Result<String> {
    canonical_hash(&json!({"state":state,"rng_seed":state.rng_seed.to_string(),
        "rng_word_pos":state.rng.get_word_pos().to_string()}))
}

impl CampaignManifest {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 1 << 20 {
            return Err("manifest exceeds 1MiB".into());
        }
        let UniqueValue(value) = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        let manifest: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        manifest.validate()?;
        Ok(manifest)
    }
    pub fn identity(&self) -> Value {
        json!({"epoch_id":self.epoch_id,"information_model":self.information_model,
            "fair_contract_version":self.fair_contract_version,"manifest_schema_version":1,"journal_schema_version":1,
            "seed":self.seed,"sampling_seed":self.sampling_seed,"deck_hashes_by_seat":self.pod,
            "effective_configs_by_seat":self.seats,"action_cap":self.action_cap})
    }
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1
            || !digest_valid(&self.game_key)
            || !digest_valid(&self.epoch_id)
            || !self.pod.iter().all(|s| digest_valid(s))
        {
            return Err("invalid manifest version or digest".into());
        }
        decimal_seed(&self.seed)?;
        match self.information_model {
            InformationModel::OmniscientStress
                if self.fair_contract_version == 0 && self.sampling_seed.is_none() => {}
            InformationModel::OpenDecklistFairV1 if self.fair_contract_version == 1 => {
                decimal_seed(
                    self.sampling_seed
                        .as_deref()
                        .ok_or("fair sampling_seed is required")?,
                )?;
            }
            _ => return Err("information model / sampling seed / fair contract mismatch".into()),
        }
        if !matches!(
            self.track.as_str(),
            "validation" | "exploration" | "tuning" | "reproduction" | "confirmation"
        ) {
            return Err("unknown track".into());
        }
        if (self.track == "exploration"
            && self.information_model != InformationModel::OmniscientStress)
            || (matches!(self.track.as_str(), "tuning" | "confirmation")
                && self.information_model != InformationModel::OpenDecklistFairV1)
        {
            return Err("track and information model do not match".into());
        }
        if self.action_cap != ACTION_CAP && !(self.track == "validation" && self.action_cap == 1) {
            return Err("action_cap must be 200000 (validation may use 1)".into());
        }
        if self.evaluated_seat.is_some_and(|s| s >= 4)
            || self.config_id.is_empty()
            || self.config_id.len() > 256
        {
            return Err("invalid config identity or evaluated seat".into());
        }
        if self
            .role
            .as_deref()
            .is_some_and(|s| !matches!(s, "candidate" | "incumbent"))
        {
            return Err("invalid comparison role".into());
        }
        if self
            .anchor_tier
            .as_deref()
            .is_some_and(|s| !matches!(s, "Medium" | "Hard" | "CEDH"))
        {
            return Err("invalid anchor tier".into());
        }
        for text in [&self.phase, &self.block_id, &self.pair_id]
            .into_iter()
            .flatten()
        {
            if text.is_empty() || text.len() > 256 || text.chars().any(char::is_control) {
                return Err("invalid membership metadata".into());
            }
        }
        self.configs()?;
        if canonical_hash(&self.identity())? != self.game_key {
            return Err("game_key does not bind manifest behavior".into());
        }
        Ok(())
    }
    pub fn configs(&self) -> Result<HashMap<PlayerId, AiConfig>> {
        let seed = decimal_seed(&self.seed)?;
        let mut configs = HashMap::new();
        for (seat, snapshot) in self.seats.iter().enumerate() {
            let difficulty: AiDifficulty = serde_json::from_value(
                snapshot
                    .get("difficulty")
                    .cloned()
                    .ok_or("missing seat difficulty")?,
            )
            .map_err(|e| e.to_string())?;
            let baseline =
                create_config_for_players(difficulty, Platform::Native, 4).into_measurement(seed);
            let mut matched = None;
            // The only modified configs are the twelve fair Medium/Hard/CEDH
            // candidates. Anchors/incumbents preserve the actual source preset.
            if self.information_model == InformationModel::OpenDecklistFairV1
                && matches!(
                    difficulty,
                    AiDifficulty::Medium | AiDifficulty::Hard | AiDifficulty::CEDH
                )
            {
                for multiplier in [1, 2] {
                    for risk in [0.4, 0.8] {
                        let mut candidate = baseline.clone();
                        candidate.search.max_nodes =
                            (candidate.search.max_nodes * multiplier).min(192);
                        candidate.profile.risk_tolerance = risk;
                        if serde_json::to_value(&candidate).map_err(|e| e.to_string())? == *snapshot
                        {
                            matched = Some(candidate);
                        }
                    }
                }
            }
            // Preserve the exact unmodified preset even if its budget exceeds the tuning clamp.
            if serde_json::to_value(&baseline).map_err(|e| e.to_string())? == *snapshot {
                matched = Some(baseline);
            }
            configs.insert(
                PlayerId(seat as u8),
                matched.ok_or("seat is not a pinned effective built-in config")?,
            );
        }
        Ok(configs)
    }
}

/// Canonical existing files only, underneath the manifest's staging directory.
fn confined_file(root: &Path, path: &Path) -> Result<PathBuf> {
    if path.components().any(|c| matches!(c, Component::ParentDir))
        || path.to_string_lossy().contains("://")
    {
        return Err("unconfined input path".into());
    }
    let full = if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    };
    let full = full.canonicalize().map_err(|e| e.to_string())?;
    if !full.starts_with(root) || !full.is_file() {
        return Err("input escapes staging".into());
    }
    Ok(full)
}

/// Stable tables use explicit replacement/removal, so changed characteristics,
/// attachments, counters and ordered zones cannot be missed by an event heuristic.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub wire_projection: bool,
    pub objects: BTreeMap<String, Value>,
    pub players: BTreeMap<String, Value>,
    pub zones: BTreeMap<String, Value>,
    pub stack: BTreeMap<String, Value>,
    pub context: Value,
    pub derived: Value,
}
impl Observation {
    pub fn capture(state: &GameState) -> Result<Self> {
        // Explicit operator-only omniscient projection; not wrap(None), which
        // intentionally removes hidden cards for an unauthenticated spectator.
        let mut wire = serde_json::to_value(ClientGameStateRef::wrap_filtered(state, state, None))
            .map_err(|e| e.to_string())?;
        let source = wire["state"]
            .as_object_mut()
            .ok_or("state projection is not an object")?;
        let mut objects: BTreeMap<String, Value> =
            serde_json::from_value(source.remove("objects").ok_or("missing objects")?)
                .map_err(|e| e.to_string())?;
        for object in objects.values_mut() {
            let face = object["name"].as_str().and_then(|name| {
                state
                    .card_db
                    .as_ref()
                    .and_then(|db| db.get_face_by_name(name))
            });
            let oracle_text = face.and_then(|face| face.oracle_text.as_deref());
            object["oracle_text"] = json!(oracle_text);
            object["card_text_available"] = json!(face.is_some());
        }
        let raw_players = source.remove("players").ok_or("missing players")?;
        let mut players = BTreeMap::new();
        let mut zones = BTreeMap::new();
        for (i, player) in raw_players
            .as_array()
            .ok_or("players are not an array")?
            .iter()
            .enumerate()
        {
            let mut player = player.clone();
            for zone in ["hand", "library", "graveyard"] {
                if let Some(value) = player.as_object_mut().and_then(|p| p.remove(zone)) {
                    zones.insert(format!("{i}:{zone}"), value);
                }
            }
            players.insert(i.to_string(), player);
        }
        for zone in ["battlefield", "exile", "command_zone", "shared_library"] {
            if let Some(value) = source.remove(zone) {
                zones.insert(zone.to_string(), value);
            }
        }
        let stack = source.remove("stack").ok_or("missing stack")?;
        let stack = stack
            .as_array()
            .ok_or("stack is not an array")?
            .iter()
            .enumerate()
            .map(|(i, v)| (i.to_string(), v.clone()))
            .collect();
        let context = Value::Object(std::mem::take(source));
        let derived = wire["derived"].take();
        let result = Self {
            wire_projection: true,
            objects,
            players,
            zones,
            stack,
            context,
            derived,
        };
        if serde_json::to_vec(&result)
            .map_err(|e| e.to_string())?
            .len()
            > OBSERVATION_LIMIT
        {
            return Err("evidence_limit/observation".into());
        }
        Ok(result)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TableDelta {
    pub upserts: BTreeMap<String, Value>,
    pub removals: Vec<String>,
}
impl TableDelta {
    fn between(before: &BTreeMap<String, Value>, after: &BTreeMap<String, Value>) -> Self {
        Self {
            upserts: after
                .iter()
                .filter(|(k, v)| before.get(*k) != Some(*v))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            removals: before
                .keys()
                .filter(|k| !after.contains_key(*k))
                .cloned()
                .collect(),
        }
    }
    fn apply(&self, table: &mut BTreeMap<String, Value>) {
        for key in &self.removals {
            table.remove(key);
        }
        table.extend(self.upserts.clone());
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObservationDelta {
    pub objects: TableDelta,
    pub players: TableDelta,
    pub zones: TableDelta,
    pub stack: TableDelta,
    pub context: Value,
    pub derived: Value,
}
impl ObservationDelta {
    pub fn between(before: &Observation, after: &Observation) -> Self {
        Self {
            objects: TableDelta::between(&before.objects, &after.objects),
            players: TableDelta::between(&before.players, &after.players),
            zones: TableDelta::between(&before.zones, &after.zones),
            stack: TableDelta::between(&before.stack, &after.stack),
            context: after.context.clone(),
            derived: after.derived.clone(),
        }
    }
    pub fn apply(&self, observation: &mut Observation) {
        self.objects.apply(&mut observation.objects);
        self.players.apply(&mut observation.players);
        self.zones.apply(&mut observation.zones);
        self.stack.apply(&mut observation.stack);
        observation.context = self.context.clone();
        observation.derived = self.derived.clone();
    }
}

#[derive(Serialize)]
struct Segment {
    path: String,
    sha256: String,
    bytes: u64,
    first_seq: u64,
    last_seq: u64,
    checkpoints: Vec<(u64, u64)>,
}

pub struct CampaignJournal {
    root: PathBuf,
    game_key: String,
    attempt_id: String,
    origin: Instant,
    file: Option<BufWriter<File>>,
    hasher: Sha256,
    segment_bytes: u64,
    total_bytes: u64,
    segment_actions: u64,
    segment_first: u64,
    seq: u64,
    last_sync: Instant,
    segments: Vec<Segment>,
    checkpoints: Vec<(u64, u64)>,
}
impl CampaignJournal {
    pub fn create(manifest: &CampaignManifest, output: &Path) -> Result<Self> {
        Self::create_with_origin(manifest, output, Instant::now())
    }

    fn create_with_origin(
        manifest: &CampaignManifest,
        output: &Path,
        origin: Instant,
    ) -> Result<Self> {
        if !output.is_absolute() {
            return Err("campaign output must be absolute".into());
        }
        std::fs::create_dir(output).map_err(|e| format!("output must be new: {e}"))?;
        let attempt_id = output
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or("invalid attempt directory")?
            .to_owned();
        if attempt_id.is_empty()
            || attempt_id.len() > 128
            || !attempt_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err("invalid attempt directory identity".into());
        }
        let mut journal = Self {
            root: output.to_owned(),
            game_key: manifest.game_key.clone(),
            attempt_id,
            origin,
            file: None,
            hasher: Sha256::new(),
            segment_bytes: 0,
            total_bytes: 0,
            segment_actions: 0,
            segment_first: 0,
            seq: 0,
            last_sync: origin,
            segments: Vec::new(),
            checkpoints: Vec::new(),
        };
        journal.open_segment()?;
        journal.append_bytes(
            &serde_json::to_vec(
                &json!({"type":"header","schema_version":1,"manifest":manifest,
            "attempt_id":journal.attempt_id,"replay_format_version":REPLAY_FORMAT_VERSION}),
            )
            .map_err(|e| e.to_string())?,
        )?;
        Ok(journal)
    }
    fn open_segment(&mut self) -> Result<()> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(
                self.root
                    .join(format!("segment-{:06}.ndjson", self.segments.len())),
            )
            .map_err(|e| e.to_string())?;
        self.file = Some(BufWriter::new(file));
        self.segment_first = self.seq;
        Ok(())
    }
    fn append_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        let file = self.file.as_mut().ok_or("journal segment is closed")?;
        file.write_all(bytes)
            .and_then(|_| file.write_all(b"\n"))
            .map_err(|e| e.to_string())?;
        self.hasher.update(bytes);
        self.hasher.update(b"\n");
        self.segment_bytes += bytes.len() as u64 + 1;
        self.total_bytes += bytes.len() as u64 + 1;
        file.flush().map_err(|e| e.to_string())?;
        if self.last_sync.elapsed() >= Duration::from_secs(1) {
            file.get_ref().sync_all().map_err(|e| e.to_string())?;
            self.last_sync = Instant::now();
        }
        Ok(())
    }
    pub fn record(&mut self, kind: &str, payload: Value, checkpoint: bool) -> Result<u64> {
        let record = json!({"schema_version":1,"game_key":self.game_key,"attempt_id":self.attempt_id,"seq":self.seq,
            "type":kind,"monotonic_ns":nanos(self.origin.elapsed()),"payload":payload});
        let bytes = serde_json::to_vec(&record).map_err(|e| e.to_string())?;
        if bytes.len() > OBSERVATION_LIMIT {
            return Err("evidence_limit/record".into());
        }
        let ceiling = if kind == "terminal" {
            JOURNAL_LIMIT
        } else {
            JOURNAL_LIMIT - TERMINAL_RESERVE
        };
        if self.total_bytes + bytes.len() as u64 + 1 > ceiling {
            return Err("evidence_limit/journal".into());
        }
        if self.file.is_none() {
            self.open_segment()?;
        }
        let seq = self.seq;
        if checkpoint {
            self.checkpoints.push((seq, self.segment_bytes));
        }
        self.append_bytes(&bytes)?;
        self.seq += 1;
        if kind == "action" {
            self.segment_actions += 1;
        }
        if kind == "terminal"
            || self.segment_bytes >= SEGMENT_BYTES
            || self.segment_actions >= CHECKPOINT_ACTIONS
        {
            self.seal()?;
        }
        Ok(seq)
    }
    fn seal(&mut self) -> Result<()> {
        let Some(mut file) = self.file.take() else {
            return Ok(());
        };
        file.flush()
            .and_then(|_| file.get_ref().sync_all())
            .map_err(|e| e.to_string())?;
        let hash = std::mem::take(&mut self.hasher).finalize();
        self.segments.push(Segment {
            path: format!("segment-{:06}.ndjson", self.segments.len()),
            sha256: format!("{hash:x}"),
            bytes: self.segment_bytes,
            first_seq: self.segment_first,
            last_seq: self.seq.saturating_sub(1),
            checkpoints: std::mem::take(&mut self.checkpoints),
        });
        self.segment_bytes = 0;
        self.segment_actions = 0;
        // A replacement index is atomic on both Windows and Unix: versioned immutable indexes.
        write_json_new(
            &self
                .root
                .join(format!("index-{:06}.json", self.segments.len())),
            &json!({"schema_version":1,"game_key":self.game_key,"attempt_id":self.attempt_id,"segments":self.segments,"next_seq":self.seq}),
        )
    }
}
fn nanos(duration: Duration) -> u64 {
    duration.as_nanos().min(u64::MAX as u128) as u64
}
fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
fn write_json_new(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    serde_json::to_writer(&mut file, value).map_err(|e| e.to_string())?;
    file.write_all(b"\n")
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())
}

#[derive(Default, Serialize)]
struct Histogram {
    buckets: BTreeMap<u32, u64>,
    count: u64,
    sum_ns: u128,
    min_ns: Option<u64>,
    max_ns: Option<u64>,
}
impl Histogram {
    fn add(&mut self, ns: u64) {
        let us = ns / 1000;
        let bucket = if us == 0 { 0 } else { 64 - us.leading_zeros() };
        *self.buckets.entry(bucket).or_default() += 1;
        self.count += 1;
        self.sum_ns += ns as u128;
        self.min_ns = Some(self.min_ns.map_or(ns, |v| v.min(ns)));
        self.max_ns = Some(self.max_ns.map_or(ns, |v| v.max(ns)));
    }
}

pub struct CampaignRecorder {
    manifest: CampaignManifest,
    journal: CampaignJournal,
    previous: Observation,
    config_ids: [String; 4],
    fair: Option<FairDecisionContext>,
    actions: u64,
    transitions: u64,
    host_transitions: u64,
    histogram: Histogram,
    recorder_ns: u128,
    card_counts: BTreeMap<(u8, String), (u64, u64)>,
    started_wall_ms: u64,
    failure: Option<(String, String)>,
    chain: String,
    eliminations: BTreeMap<u8, u32>,
    unresolved_exposure_events: u64,
}
impl CampaignRecorder {
    pub fn create(
        manifest: CampaignManifest,
        output: &Path,
        state: &GameState,
        db: Arc<CardDatabase>,
        header: &ReplayHeader,
        initial_events: &[GameEvent],
    ) -> Result<Self> {
        let started_wall_ms = wall_ms();
        let start = Instant::now();
        let previous = Observation::capture(state)?;
        let fair = manifest
            .sampling_seed
            .as_deref()
            .map(decimal_seed)
            .transpose()?
            .map(|seed| FairDecisionContext::new(seed).with_card_database(db));
        let mut journal = CampaignJournal::create_with_origin(&manifest, output, start)?;
        write_json_new(&output.join("reproduce.json"), &manifest)?;
        let hash = deterministic_state_hash(state)?;
        let observation_hash =
            canonical_hash(&serde_json::to_value(&previous).map_err(|e| e.to_string())?)?;
        journal.record("initial",json!({"checkpoint":previous,"state_hash":hash,"observation_hash":observation_hash,"replay_header":header,"events":initial_events}),true)?;
        let [a, b, c, d] = manifest.seats.each_ref().map(canonical_hash);
        let config_ids = [a?, b?, c?, d?];
        let mut recorder = Self {
            manifest,
            journal,
            previous,
            fair,
            actions: 0,
            transitions: 0,
            histogram: Histogram::default(),
            recorder_ns: 0,
            card_counts: BTreeMap::new(),
            started_wall_ms,
            failure: None,
            chain: hash,
            eliminations: BTreeMap::new(),
            unresolved_exposure_events: 0,
            host_transitions: 0,
            config_ids,
        };
        recorder.exposure(state, initial_events);
        recorder.recorder_ns = start.elapsed().as_nanos();
        Ok(recorder)
    }
    fn transition(
        &mut self,
        state: &GameState,
        events: &[GameEvent],
        action: Option<RecordedAction>,
        owner: Option<PlayerId>,
        log: Value,
    ) -> Result<()> {
        let start = Instant::now();
        let next = Observation::capture(state)?;
        let state_hash = deterministic_state_hash(state)?;
        let observation_hash =
            canonical_hash(&serde_json::to_value(&next).map_err(|e| e.to_string())?)?;
        let checkpoint = action.is_some() && self.actions.is_multiple_of(CHECKPOINT_ACTIONS);
        let observation = if checkpoint {
            json!({"checkpoint":next})
        } else {
            json!({"delta":ObservationDelta::between(&self.previous,&next)})
        };
        let deterministic = json!({"previous":self.chain,"state_hash":state_hash,"action":action,"events":events,"semantic_owner":owner});
        let hash = canonical_hash(&deterministic)?;
        let actor = action.as_ref().map(|a| a.actor);
        self.journal.record("action",json!({"actor":actor,"semantic_owner":owner,"recorded_action":action,
            "host_transition":if actor.is_none() { Some("headless_contest") } else { None },"events":events,"log_entries":log,
            "spectator":observation,"state_hash":state_hash,"observation_hash":observation_hash,"deterministic_hash":hash,"capture_wall_ms":wall_ms()}),checkpoint)?;
        self.transitions += 1;
        self.exposure(state, events);
        for player in &state.players {
            if player.is_eliminated {
                self.eliminations
                    .entry(player.id.0)
                    .or_insert(state.turn_number);
            }
        }
        self.previous = next;
        self.chain = hash;
        self.recorder_ns += start.elapsed().as_nanos();
        Ok(())
    }
    fn exposure(&mut self, state: &GameState, events: &[GameEvent]) {
        for event in events {
            let (player, object, draw) = match event {
                GameEvent::CardDrawn {
                    player_id,
                    object_id,
                    ..
                } => (*player_id, *object_id, true),
                GameEvent::SpellCast {
                    controller,
                    object_id,
                    ..
                } => (*controller, *object_id, false),
                _ => continue,
            };
            let name = state
                .objects
                .get(&object)
                .map(|o| o.name.clone())
                .or_else(|| {
                    self.previous
                        .objects
                        .get(&object.0.to_string())
                        .and_then(|o| o["name"].as_str())
                        .map(str::to_owned)
                });
            if let Some(name) = name {
                let counts = self.card_counts.entry((player.0, name)).or_default();
                if draw {
                    counts.0 += 1;
                } else {
                    counts.1 += 1;
                }
            } else {
                self.unresolved_exposure_events += 1;
            }
        }
    }
    pub fn finish(mut self, state: &GameState, outcome: &str, stop: Value) -> Result<Value> {
        let start = Instant::now();
        let winner = if let WaitingFor::GameOver { winner } = &state.waiting_for {
            *winner
        } else {
            None
        };
        let seats:Vec<_> = state.players.iter().map(|p| {
            let mut zones:BTreeMap<engine::types::zones::Zone,u64> = BTreeMap::new();
            for (_,object) in state.objects.iter().filter(|(_,object)|object.owner == p.id) {
                *zones.entry(object.zone).or_default() += 1;
            }
            json!({"seat":p.id,"life":p.life,"eliminated":p.is_eliminated,"eliminated_turn":self.eliminations.get(&p.id.0),
                "hand":p.hand.len(),"library":state.library_of(p.id).len(),"graveyard":p.graveyard.len(),"zones":zones})
        }).collect();
        let cards:Vec<_> = self.card_counts.iter().map(|((seat,name),(draws,casts))|json!({"seat":seat,"name":name,"draws":draws,"casts":casts})).collect();
        let incomplete = !matches!(outcome, "won" | "draw");
        if !matches!(outcome, "won" | "draw" | "capped") {
            // The live RNG is serde-skipped. Synchronize its continuation on
            // the owned persistence snapshot, never mutate the observed game.
            let mut restore_state = state.clone();
            restore_state.capture_rng_word_pos();
            write_json_new(
                &self.journal.root.join("restore.json"),
                &json!({"gameState":PersistedGameState::capture(restore_state),
                "engine_seed":self.manifest.seed,"sampling_seed":self.manifest.sampling_seed,"decisions":self.histogram.count,
                "reexecution":"reproduce.json; restore is engine state, chooser RNG is reproduced from the seed and action prefix"}),
            )?;
        }
        let resources = cgroup_resources();
        let result_hash = canonical_hash(
            &json!({"trace":self.chain,"outcome":outcome,"stop":stop,"actions":self.actions,"state_hash":deterministic_state_hash(state)?}),
        )?;
        // Freeze both duration and overhead at the same observation boundary.
        // Final record encoding/fsync happens after this payload; post-exit
        // process accounting belongs to the collector's separate receipt.
        let terminal_instant = Instant::now();
        let terminal_wall_ms = wall_ms();
        let duration_ns = nanos(terminal_instant.duration_since(self.journal.origin));
        let recorder_ns = self.recorder_ns + terminal_instant.duration_since(start).as_nanos();
        let terminal = json!({"schema_version":1,"game_key":self.manifest.game_key,"attempt_id":self.journal.attempt_id,
            "outcome":outcome,"winner":winner,"turns":state.turn_number,"actions":self.actions,
            "host_transitions":self.host_transitions,"recorded_transitions":self.transitions,"result_hash":result_hash,
            "decisions":self.histogram.count,"decision_latency":self.histogram,"recorder_ns":recorder_ns,
            "per_seat":seats,"per_card":cards,"unresolved_exposure_events":self.unresolved_exposure_events,"stop":stop,"incomplete":incomplete,"deterministic_hash":self.chain,
            "started_wall_time":self.started_wall_ms,"terminal_wall_time":terminal_wall_ms,"wall_time_format":"unix_ms_utc","duration_ns":duration_ns,
            "timing_scope":"recorder_initialization_through_terminal_payload","terminal_encoding_included":false,
            "cpu_seconds":resources.0,"peak_rss_bytes":resources.1,"resource_accounting":"cgroup_v2_or_unavailable",
            "resource_sample":"native_pre_exit","collector_resource_finalization_required":true});
        self.journal.record("terminal", terminal.clone(), false)?;
        write_json_new(&self.journal.root.join("terminal.json"), &terminal)?;
        Ok(terminal)
    }
}
impl DriverHooks for CampaignRecorder {
    fn choose(
        &mut self,
        state: &GameState,
        config: &AiConfig,
        contract: &AiDecisionContract,
        rng: &mut impl Rng,
        session: Option<&Arc<AiSession>>,
    ) -> Result<Option<GameAction>> {
        if self.journal.origin.elapsed() >= Duration::from_secs(1800) {
            self.failure = Some(("timeout".into(), "deadline".into()));
            return Err("deadline".into());
        }
        let (action, materialize_ns, chooser_ns) = if let Some(context) = &mut self.fair {
            match context.choose(
                state,
                config,
                contract.semantic_owner,
                contract.authorized_actor,
            ) {
                Ok(decision) => (
                    decision.action,
                    decision.materialize_ns,
                    decision.chooser_ns,
                ),
                Err(error) => {
                    self.failure = Some((error.outcome().to_owned(), error.code().to_owned()));
                    return Err(error.to_string());
                }
            }
        } else {
            let start = Instant::now();
            let action = crate::search::choose_action_with_issued_contract(
                state,
                config,
                rng,
                session.ok_or("missing omniscient session")?,
                contract,
            );
            (action, 0, nanos(start.elapsed()))
        };
        let start = Instant::now();
        let latency = materialize_ns.saturating_add(chooser_ns);
        self.histogram.add(latency);
        let config_id = &self.config_ids[usize::from(contract.authorized_actor.0)];
        self.journal.record("decision",json!({"actor":contract.authorized_actor,"semantic_owner":contract.semantic_owner,
            "decision_kind":state.waiting_for.variant_name(),"config_id":config_id,"information_model":self.manifest.information_model,
            "materialize_ns":materialize_ns,"chooser_ns":chooser_ns,"latency_ns":latency}),false)?;
        self.recorder_ns += start.elapsed().as_nanos();
        Ok(action)
    }
    fn applied(
        &mut self,
        actor: PlayerId,
        semantic_owner: PlayerId,
        verified_pass: bool,
        result: &AiActionResult,
    ) -> Result<()> {
        let action = RecordedAction {
            seq: self.actions as u32,
            actor,
            action: result.action.clone(),
            kind: if verified_pass {
                RecordedActionKind::VerifiedAiPriorityPass { semantic_owner }
            } else {
                RecordedActionKind::Submitted
            },
        };
        self.actions += 1;
        self.transition(
            &result.state,
            &result.events,
            Some(action),
            Some(semantic_owner),
            serde_json::to_value(&result.log_entries).map_err(|e| e.to_string())?,
        )
    }
    fn host_settled(&mut self, state: &GameState, events: &[GameEvent]) -> Result<()> {
        self.host_transitions += 1;
        self.transition(state, events, None, None, json!([]))
    }
}

pub fn build_game_state(
    db: &CardDatabase,
    payload: &DeckPayload,
    players: u8,
    seed: u64,
) -> GameState {
    let mut state = GameState::new(FormatConfig::commander(), players, seed);
    load_and_hydrate_decks(&mut state, payload, Some(db));
    state
}

/// Drive a real initialized fixture or staged game. No alternate outcome path.
pub fn run_game(
    manifest: CampaignManifest,
    output: &Path,
    db: Arc<CardDatabase>,
    mut state: GameState,
    header: ReplayHeader,
    initial_events: &[GameEvent],
) -> Result<Value> {
    manifest.validate()?;
    let configs = manifest.configs()?;
    let ai_players: HashSet<_> = (0..4).map(PlayerId).collect();
    let session = (manifest.information_model == InformationModel::OmniscientStress)
        .then(|| AiSession::arc_from_game(&state));
    let mut rng = rand::rngs::StdRng::seed_from_u64(decimal_seed(&manifest.seed)?);
    let cap = manifest.action_cap;
    let mut recorder =
        CampaignRecorder::create(manifest, output, &state, db, &header, initial_events)?;
    let driven = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_driver_loop_observed(
            &mut state,
            &ai_players,
            &configs,
            &mut rng,
            session.as_ref(),
            cap,
            &mut recorder,
        )
    }));
    let (outcome, stop) = match driven {
        Err(panic) => {
            let message = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("non-string panic");
            (
                "panicked".to_string(),
                json!({"category":"panic","code":"native_unwind","message":message}),
            )
        }
        Ok(Err(error)) => {
            let (outcome, code) = recorder.failure.take().unwrap_or_else(|| {
                (
                    "infrastructure_error".into(),
                    if error.starts_with("evidence_limit/") {
                        "evidence_limit".into()
                    } else {
                        "journal_io".into()
                    },
                )
            });
            (outcome, json!({"code":code,"message":error}))
        }
        Ok(Ok(result)) => match &state.waiting_for {
            WaitingFor::GameOver { winner } => (
                if winner.is_some() { "won" } else { "draw" }.into(),
                Value::Null,
            ),
            _ => match result.exit {
                DriverExit::CapReached => ("capped".into(), json!({"code":"action_cap"})),
                DriverExit::BatchBreak(AiActionsStop::ApplyFailed {
                    player,
                    action,
                    error,
                }) => (
                    "illegal_action".into(),
                    json!({"code":"contract_violation","actor":player,"action":action,"error":error.to_string()}),
                ),
                DriverExit::BatchBreak(stop) => ("stalled".into(), stop_payload(&stop)),
            },
        },
    };
    recorder.finish(&state, &outcome, stop)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Corpus {
    schema_version: u32,
    identity_scheme_version: u32,
    identity_fallbacks: Vec<String>,
    decks: Vec<CorpusDeck>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CorpusDeck {
    deck_id: String,
    main_deck: Vec<String>,
    commander: Vec<String>,
    companion: Vec<String>,
}

/// Native entry accepts only local pinned normalized inputs. Dependency download,
/// resource accounting and network/credential isolation belong to the worker sandbox.
pub fn run_manifest(manifest_path: &Path, output: &Path) -> Result<Value> {
    let manifest_path = manifest_path.canonicalize().map_err(|e| e.to_string())?;
    let root = manifest_path
        .parent()
        .ok_or("manifest needs staging directory")?;
    let bytes = read_bounded(&manifest_path, 1 << 20)?;
    let mut manifest = CampaignManifest::parse(&bytes)?;
    let parent = output
        .parent()
        .ok_or("output needs parent directory")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !output.is_absolute()
        || !parent.starts_with(root)
        || output
            .components()
            .any(|c| matches!(c, Component::ParentDir))
    {
        return Err("output escapes staging".into());
    }
    let export_path = confined_file(root, &manifest.export_path)?;
    let feed_path = confined_file(root, &manifest.feed_path)?;
    // Reproduction tooling stages these same relative inputs beside the copied
    // manifest; absolute paths must never bind replay to the old machine.
    manifest.export_path = export_path
        .strip_prefix(root)
        .map_err(|e| e.to_string())?
        .to_owned();
    manifest.feed_path = feed_path
        .strip_prefix(root)
        .map_err(|e| e.to_string())?
        .to_owned();
    let db = Arc::new(CardDatabase::from_export(&export_path).map_err(|e| e.to_string())?);
    let feed = read_bounded(&feed_path, 16 << 20)?;
    let UniqueValue(value) = serde_json::from_slice(&feed).map_err(|e| e.to_string())?;
    let corpus: Corpus = serde_json::from_value(value).map_err(|e| e.to_string())?;
    if corpus.schema_version != 1
        || corpus.identity_scheme_version != 1
        || corpus.decks.is_empty()
        || corpus.decks.len() > 200
    {
        return Err("unsupported corpus schema or deck count".into());
    }
    let expected_fallbacks = corpus_fallbacks(
        &db,
        corpus
            .decks
            .iter()
            .flat_map(|d| d.main_deck.iter().chain(&d.commander).chain(&d.companion)),
    )?;
    if corpus.identity_fallbacks != expected_fallbacks {
        return Err("corpus identity fallback diagnostics mismatch".into());
    }
    let mut by_id = BTreeMap::new();
    for deck in &corpus.decks {
        if !digest_valid(&deck.deck_id) || by_id.insert(&deck.deck_id, deck).is_some() {
            return Err("invalid or duplicate corpus deck identity".into());
        }
        if normalized_deck_id(&db, &deck.main_deck, &deck.commander, &deck.companion)?
            != deck.deck_id
        {
            return Err("corpus deck_id does not bind normalized resolved card counts".into());
        }
    }
    let mut decks = Vec::new();
    for id in &manifest.pod {
        let deck = by_id.get(id).ok_or("pod deck absent from pinned corpus")?;
        if deck.main_deck.len() + deck.commander.len() != 100
            || deck.commander.is_empty()
            || deck.commander.len() > 2
            || deck.companion.len() > 1
        {
            return Err("invalid Commander deck counts".into());
        }
        decks.push(PlayerDeckList {
            main_deck: canonical_deck_section(&db, &deck.main_deck)?,
            commander: canonical_deck_section(&db, &deck.commander)?,
            companion: canonical_deck_section(&db, &deck.companion)?,
            ..Default::default()
        });
    }
    let deck_list = DeckList {
        player: decks.remove(0),
        opponent: decks.remove(0),
        ai_decks: decks,
        ..Default::default()
    };
    let seed = decimal_seed(&manifest.seed)?;
    let mut state = build_game_state(&db, &resolve_deck_list(&db, &deck_list), 4, seed);
    let header = ReplayHeader {
        format_config: FormatConfig::commander(),
        match_config: Default::default(),
        player_count: 4,
        first_player: None,
        seed,
        deck_data: Some(deck_list),
    };
    let initial = engine::game::engine::start_game(&mut state);
    run_game(manifest, output, db, state, header, &initial.events)
}

pub fn capabilities() -> Value {
    json!({"protocol_version":1,"manifest_schema_version":1,"journal_schema_version":1,"telemetry_schema_version":1,
        "fair_contract_version":1,"information_models":["omniscient_stress","open_decklist_fair_v1"],"player_count":4,
        "action_cap":ACTION_CAP,"validation_action_cap":1,"observation_limit_bytes":OBSERVATION_LIMIT,"journal_limit_bytes":JOURNAL_LIMIT,
        "corpus_schema_version":1,"config_catalog_schema_version":1,"replay_format_version":REPLAY_FORMAT_VERSION})
}

/// Epoch preparation exports the native preset authority once. The sole
/// seed-dependent field is declared, so scheduling never reimplements presets.
pub fn config_catalog() -> Result<Value> {
    let mut anchors = Vec::with_capacity(3);
    let mut candidates = Vec::with_capacity(12);
    for (tier, difficulty) in [
        ("medium", AiDifficulty::Medium),
        ("hard", AiDifficulty::Hard),
        ("cedh", AiDifficulty::CEDH),
    ] {
        let baseline =
            create_config_for_players(difficulty, Platform::Native, 4).into_measurement(0);
        let snapshot = serde_json::to_value(&baseline).map_err(|error| error.to_string())?;
        let mut seeded = serde_json::to_value(
            create_config_for_players(difficulty, Platform::Native, 4).into_measurement(u64::MAX),
        )
        .map_err(|error| error.to_string())?;
        let field = seeded
            .pointer_mut("/execution_mode/Measurement/seed")
            .ok_or("unsupported measurement seed contract")?;
        *field = json!(0);
        if seeded != snapshot {
            return Err("effective config has unsupported seed-dependent fields".into());
        }
        anchors.push(
            json!({"id":format!("{tier}-base"),"effective_config":snapshot,
            "config_hash":canonical_hash(&snapshot)?}),
        );
        for multiplier in [1, 2] {
            for (risk, suffix) in [(0.4, "04"), (0.8, "08")] {
                let mut candidate = baseline.clone();
                candidate.search.max_nodes = (candidate.search.max_nodes * multiplier).min(192);
                candidate.profile.risk_tolerance = risk;
                let effective =
                    serde_json::to_value(candidate).map_err(|error| error.to_string())?;
                candidates.push(json!({"id":format!("{tier}-n{multiplier}-r{suffix}"),
                    "effective_config":effective,"config_hash":canonical_hash(&effective)?}));
            }
        }
    }
    Ok(
        json!({"schema_version":1,"player_count":4,"template_seed":"0",
        "measurement_seed_pointer":"/execution_mode/Measurement/seed",
        "anchors":anchors,"candidates":candidates}),
    )
}

/// Tagged resolved face identities distinguish alternate faces sharing an Oracle
/// UUID. Missing Oracle metadata uses the engine's canonical name authority.
pub fn normalized_deck_id(
    db: &CardDatabase,
    main: &[String],
    commander: &[String],
    companion: &[String],
) -> Result<String> {
    let counts = |names: &[String]| -> Result<BTreeMap<String, u32>> {
        let mut result = BTreeMap::new();
        for name in names {
            let (id, _) = resolved_card_identity(db, name)?;
            *result.entry(id).or_default() += 1;
        }
        Ok(result)
    };
    canonical_hash(
        &json!({"main":counts(main)?,"commander":counts(commander)?,"companion":counts(companion)?}),
    )
}

fn resolved_card_identity(db: &CardDatabase, name: &str) -> Result<(String, String)> {
    let face = db
        .get_face_by_name(name)
        .ok_or_else(|| format!("unresolved corpus card: {name}"))?;
    let canonical_name = face.name.to_lowercase();
    let id = match &face.scryfall_oracle_id {
        Some(oracle) => format!("oracle:{oracle}:{canonical_name}"),
        None => format!("name:{canonical_name}"),
    };
    Ok((id, face.name.clone()))
}

/// Canonicalize before seeded shuffling: input permutation cannot change a case.
pub fn canonical_deck_section(db: &CardDatabase, names: &[String]) -> Result<Vec<String>> {
    let mut identities = names
        .iter()
        .map(|name| resolved_card_identity(db, name))
        .collect::<Result<Vec<_>>>()?;
    identities.sort();
    Ok(identities.into_iter().map(|(_, name)| name).collect())
}

/// Generate actual local inputs, not canned results. The ordinary campaign
/// command runs these decks through the engine until deck exhaustion or cap.
pub fn write_smoke_inputs(directory: &Path) -> Result<Value> {
    if !directory.is_absolute() {
        return Err("fixture directory must be absolute".into());
    }
    std::fs::create_dir(directory).map_err(|e| e.to_string())?;
    let common = json!({"mana_cost":{"type":"NoCost"},"power":null,"toughness":null,"loyalty":null,"defense":null,
        "oracle_text":null,"non_ability_text":null,"flavor_name":null,"keywords":[],"abilities":[],"triggers":[],
        "static_abilities":[],"replacements":[],"color_override":null,"scryfall_oracle_id":null});
    let mut commander = common.clone();
    commander["name"] = json!("Campaign Fixture Commander");
    commander["card_type"] =
        json!({"supertypes":["Legendary"],"core_types":["Creature"],"subtypes":["Human"]});
    commander["power"] = json!({"type":"Fixed","value":2});
    commander["toughness"] = json!({"type":"Fixed","value":2});
    let mut land = common;
    land["name"] = json!("Campaign Fixture Land");
    land["card_type"] = json!({"supertypes":["Basic"],"core_types":["Land"],"subtypes":["Plains"]});
    let cards = json!({"campaign fixture commander":commander,"campaign fixture land":land});
    write_json_new(&directory.join("card-data.json"), &cards)?;
    let db = CardDatabase::from_json_str(&cards.to_string()).map_err(|e| e.to_string())?;
    let main = vec!["Campaign Fixture Land".to_string(); 99];
    let commanders = vec!["Campaign Fixture Commander".to_string()];
    let deck_id = normalized_deck_id(&db, &main, &commanders, &[])?;
    write_json_new(
        &directory.join("corpus.json"),
        &json!({"schema_version":1,"identity_scheme_version":1,
        "identity_fallbacks":corpus_fallbacks(&db,main.iter().chain(&commanders))?,"decks":[{
        "deck_id":deck_id,"main_deck":main,"commander":commanders,"companion":[]}]}),
    )?;
    let config = serde_json::to_value(
        create_config_for_players(AiDifficulty::Easy, Platform::Native, 4).into_measurement(42),
    )
    .map_err(|e| e.to_string())?;
    let mut manifest = CampaignManifest {
        schema_version: 1,
        game_key: String::new(),
        epoch_id: canonical_hash(&json!({"fixture_version":1,"cards":cards,"deck":deck_id}))?,
        seed: "42".into(),
        sampling_seed: None,
        track: "validation".into(),
        phase: None,
        panel_id: None,
        block_id: None,
        pair_id: None,
        role: None,
        anchor_tier: None,
        config_id: canonical_hash(&config)?,
        evaluated_seat: None,
        information_model: InformationModel::OmniscientStress,
        fair_contract_version: 0,
        pod: std::array::from_fn(|_| deck_id.clone()),
        seats: std::array::from_fn(|_| config.clone()),
        action_cap: 1,
        export_path: "card-data.json".into(),
        feed_path: "corpus.json".into(),
    };
    manifest.game_key = canonical_hash(&manifest.identity())?;
    write_json_new(&directory.join("capped.json"), &manifest)?;
    manifest.action_cap = ACTION_CAP;
    manifest.game_key = canonical_hash(&manifest.identity())?;
    write_json_new(&directory.join("complete.json"), &manifest)?;
    Ok(
        json!({"fixture_only":true,"directory":directory,"manifests":["capped.json","complete.json"],
        "execution":"ai-commander --campaign-game <manifest> --campaign-output <new-directory-within-staging>"}),
    )
}

fn stop_payload(stop: &AiActionsStop) -> Value {
    match stop {
        AiActionsStop::NoEligibleAiActor => json!({"code":"no_eligible_ai_actor"}),
        AiActionsStop::MissingAiConfig { player } => {
            json!({"code":"missing_ai_config","actor":player})
        }
        AiActionsStop::ChooseActionNone { player } => {
            json!({"code":"choose_action_none","actor":player})
        }
        AiActionsStop::ApplyFailed {
            player,
            action,
            error,
        } => {
            json!({"code":"contract_violation","actor":player,"action":action,"error":error.to_string()})
        }
        AiActionsStop::ActionSafetyCapReached { limit } => {
            json!({"code":"action_safety_cap","limit":limit})
        }
        AiActionsStop::ActionBudgetReached { limit } => {
            json!({"code":"action_budget","limit":limit})
        }
    }
}

/// Never estimates process resource use. The collector takes the final sample
/// after exit; this sample is useful while a terminal record is being sealed.
fn cgroup_resources() -> (Option<f64>, Option<u64>) {
    #[cfg(target_os = "linux")]
    {
        let read = || -> Option<(Option<f64>, Option<u64>)> {
            let membership = std::fs::read_to_string("/proc/self/cgroup").ok()?;
            let path = membership
                .lines()
                .find_map(|line| line.strip_prefix("0::"))?;
            if Path::new(path)
                .components()
                .any(|part| matches!(part, Component::ParentDir))
            {
                return None;
            }
            let group = Path::new("/sys/fs/cgroup").join(path.trim_start_matches('/'));
            if !group
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with("game-"))
            {
                return None;
            }
            let peak = std::fs::read_to_string(group.join("memory.peak"))
                .ok()
                .and_then(|s| s.trim().parse().ok());
            let cpu = std::fs::read_to_string(group.join("cpu.stat"))
                .ok()
                .and_then(|s| {
                    s.lines().find_map(|line| {
                        line.strip_prefix("usage_usec ")
                            .and_then(|v| v.parse::<u64>().ok())
                    })
                })
                .map(|us| us as f64 / 1_000_000.0);
            Some((cpu, peak))
        };
        read().unwrap_or((None, None))
    }
    #[cfg(not(target_os = "linux"))]
    {
        (None, None)
    }
}

fn read_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > maximum {
        return Err("input exceeds byte limit".into());
    }
    Ok(bytes)
}

fn corpus_fallbacks<'a>(
    db: &CardDatabase,
    names: impl Iterator<Item = &'a String>,
) -> Result<Vec<String>> {
    let mut fallbacks = BTreeSet::new();
    for name in names {
        let (id, _) = resolved_card_identity(db, name)?;
        if id.starts_with("name:") {
            fallbacks.insert(id);
        }
    }
    Ok(fallbacks.into_iter().collect())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NormalizationRequests {
    schema_version: u32,
    decks: Vec<NormalizationDeck>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NormalizationDeck {
    main_deck: Vec<String>,
    commander: Vec<String>,
    companion: Vec<String>,
}

/// Epoch preparation uses this exact native resolver rather than duplicating
/// alias/alternate-face handling in Python. Output is new and stage confined.
pub fn normalize_corpus(export: &Path, requests: &Path, output: &Path) -> Result<Value> {
    let requests = requests.canonicalize().map_err(|e| e.to_string())?;
    let root = requests.parent().ok_or("requests need staging directory")?;
    let export = confined_file(root, export)?;
    let parent = output
        .parent()
        .ok_or("output needs parent")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !output.is_absolute()
        || !parent.starts_with(root)
        || output
            .components()
            .any(|c| matches!(c, Component::ParentDir))
    {
        return Err("normalization output escapes staging".into());
    }
    let UniqueValue(value) =
        serde_json::from_slice(&read_bounded(&requests, 16 << 20)?).map_err(|e| e.to_string())?;
    let requests: NormalizationRequests =
        serde_json::from_value(value).map_err(|e| e.to_string())?;
    if requests.schema_version != 1 || requests.decks.is_empty() || requests.decks.len() > 200 {
        return Err("unsupported normalization schema or deck count".into());
    }
    let db = CardDatabase::from_export(&export).map_err(|e| e.to_string())?;
    let mut decks = BTreeMap::new();
    let fallbacks = corpus_fallbacks(
        &db,
        requests
            .decks
            .iter()
            .flat_map(|d| d.main_deck.iter().chain(&d.commander).chain(&d.companion)),
    )?;
    for deck in requests.decks {
        if deck.main_deck.len() + deck.commander.len() != 100
            || deck.commander.is_empty()
            || deck.commander.len() > 2
            || deck.companion.len() > 1
        {
            return Err("invalid Commander deck counts; normalization never repairs decks".into());
        }
        let main = canonical_deck_section(&db, &deck.main_deck)?;
        let commander = canonical_deck_section(&db, &deck.commander)?;
        let companion = canonical_deck_section(&db, &deck.companion)?;
        let id = normalized_deck_id(&db, &main, &commander, &companion)?;
        let row =
            json!({"deck_id":id,"main_deck":main,"commander":commander,"companion":companion});
        if decks.insert(id, row).is_some() {
            return Err("duplicate normalized deck".into());
        }
    }
    let corpus = json!({"schema_version":1,"identity_scheme_version":1,"identity_fallbacks":fallbacks,
        "decks":decks.into_values().collect::<Vec<_>>()});
    let bytes = canonical_bytes(&corpus)?;
    if bytes.len() > 16 << 20 {
        return Err("normalized corpus exceeds 16MiB".into());
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(
        json!({"schema_version":1,"output":output,"sha256":format!("{:x}",Sha256::digest(bytes)),
        "deck_count":corpus["decks"].as_array().map(Vec::len),"identity_scheme_version":1,"identity_fallbacks":corpus["identity_fallbacks"]}),
    )
}
