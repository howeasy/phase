import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { CardPrinting, GameObject, GameState } from "../../../adapter/types.ts";
import type * as Platform from "../../../services/platform.ts";
import type { PrintingEntry } from "../../../services/scryfall.ts";
import type { ResolutionKey, ResolutionResponse, ResolvedAsset } from "../../../services/visualPacks/types.ts";
import { assetKey, catalogRoot, installedRevision, packId } from "../../../services/visualPacks/types.ts";
import { encodeCandidateKey } from "../../../services/visualPacks/candidateKeys.ts";
import type { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import type { useGameStore } from "../../../stores/gameStore.ts";
import type { useUiStore } from "../../../stores/uiStore.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { gameStateFactory, buildStackEntry } from "../../../test/factories/gameStateFactory.ts";

// Only unrelated engine-content and dispatch boundaries are doubled. Image
// selection, repositories, preference selection, and Settings remain real.
const dispatch = vi.hoisted(() => vi.fn().mockResolvedValue(undefined));
vi.mock("../../../game/dispatch.ts", () => ({ dispatchAction: dispatch, dispatchInteraction: vi.fn() }));
vi.mock("../../../hooks/useGameDispatch.ts", () => ({ useGameDispatch: () => dispatch }));
vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useEngineCardData: () => null,
  useLocalizedCardName: (name: string | null) => name,
  useCardParseDetails: () => null,
  useCardRulings: () => [],
}));
vi.mock("../../../hooks/useLocalizedCardContent.ts", () => ({ useLocalizedCardContent: () => null }));
vi.mock("../../../services/backup.ts", () => ({ downloadBackup: vi.fn(), importBackupFromFile: vi.fn() }));

const ORACLE = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const OTHER_ORACLE = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const OWNER_A = "11111111-1111-4111-8111-111111111111";
const OWNER_B = "22222222-2222-4222-8222-222222222222";
const VIEWER = "33333333-3333-4333-8333-333333333333";
const FOREIGN = "44444444-4444-4444-8444-444444444444";
const printingA: CardPrinting = { set_code: "own", collector_number: "1" };
const printingB: CardPrinting = { set_code: "own", collector_number: "2" };
const foreignPrinting: CardPrinting = { set_code: "foreign", collector_number: "9" };
const URL_A = "/owner-a.jpg";
const URL_B = "/owner-b.jpg";
const URL_V = "/viewer.jpg";
const CROP_A = "/owner-a-crop.jpg";
const CROP_V = "/viewer-crop.jpg";

function json(value: unknown) {
  return new Response(JSON.stringify(value), { headers: { "Content-Type": "application/json" } });
}

function printing(id: string, set: string, collector_number: string): PrintingEntry {
  return {
    id, set, collector_number, set_name: set, released_at: "2026-01-01", border_color: "black",
    frame_effects: [], full_art: false,
    faces: [{ normal: `https://img.example/${id}.jpg`, art_crop: `https://img.example/${id}-crop.jpg` }],
  };
}

function card(overrides: Partial<GameObject> = {}) {
  return gameObjectFactory.creature(2, 2).onBattlefield().named("Alpha").build({
    printed_ref: { oracle_id: ORACLE, face_name: "Alpha" },
    printing: printingA, display_visible_to_viewer: true, ...overrides,
  });
}
// Dynamic imports intentionally reload the real resolver/store graph after
// resetModules: static imports would retain repositories across fixture runs.
interface Fixture {
  preferences: typeof usePreferencesStore;
  store: typeof useGameStore;
  ui: typeof useUiStore;
  seed: (objects: GameObject[], overrides?: Partial<GameState>) => GameState;
  admit: () => void;
  reset: () => void;
}
let activeFixture: Fixture | undefined;

async function setup({ holdCatalog = false } = {}): Promise<Fixture> {
  const entry = {
    oracle_id: ORACLE, name: "Alpha", face_names: ["alpha"], layout: "normal",
    mana_cost: "{1}", cmc: 1, type_line: "Creature", colors: [], color_identity: [], keywords: [],
    faces: [{ normal: "/canonical.jpg", art_crop: "/canonical-crop.jpg" }],
  };
  const catalog = {
    [ORACLE]: [printing(OWNER_A, "own", "1"), printing(OWNER_B, "own", "2"), printing(VIEWER, "pin", "3")],
    [OTHER_ORACLE]: [printing(FOREIGN, "foreign", "9")],
  };
  let admit!: (response: Response) => void;
  const gate = new Promise<Response>((resolve) => { admit = resolve; });
  const installed = new Map<string, ResolvedAsset[]>();
  const backend = {
    resolve: vi.fn(async (keys: ResolutionKey[]): Promise<ResolutionResponse> => ({
      revision: installedRevision("0"),
      entries: keys.map((key, ordinal) => ({ ordinal, key, matches: installed.get(key.key) ?? [] })),
    })),
    subscribeRevision: vi.fn(async () => () => {}),
  };
  vi.doMock("../../../services/platform.ts", async (original) => ({
    ...await original<typeof Platform>(), loadVisualPackBackend: vi.fn(async () => backend),
  }));
  vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
    const url = new URL(String(input), "http://localhost/");
    if (url.pathname === "/scryfall-data.json") return json({ [ORACLE]: entry, alpha: entry });
    if (url.pathname === "/scryfall-printings.json") return holdCatalog ? gate : json(catalog);
    throw new Error(`Unexpected fixture request ${String(input)}`);
  }));
  const preferences = (await import("../../../stores/preferencesStore.ts")).usePreferencesStore;
  preferences.setState({ ...preferences.getInitialState(), language: "en", showOwnerPrintings: true,
    artOverrides: { [ORACLE]: { scryfallId: VIEWER, setCode: "pin", collectorNumber: "3" } }, artChain: [] });
  const connectivity = (await import("../../../stores/connectivityStore.ts")).useConnectivityStore;
  connectivity.setState({ forcedOffline: false, browserOnline: true });
  for (const [id, full, crop] of [[OWNER_A, URL_A, CROP_A], [OWNER_B, URL_B, "/owner-b-crop.jpg"], [VIEWER, URL_V, CROP_V]]) {
    for (const rung of ["small", "normal", "art_crop"] as const) {
      const variant = rung === "art_crop" ? "art_crop" : "full_card";
      const key = encodeCandidateKey("english_printing", [id, 0, variant, rung]);
      installed.set(key, [{ packId: packId("complete"), assetKey: assetKey("asset:v1:exact_printing:QQ"),
        catalogRoot: catalogRoot("a".repeat(64)), url: rung === "art_crop" ? crop : full, media: "image/jpeg" }]);
    }
  }
  const service = await import("../../../services/scryfall.ts");
  await service.loadScryfallData();
  const store = (await import("../../../stores/gameStore.ts")).useGameStore;
  const ui = (await import("../../../stores/uiStore.ts")).useUiStore;
  ui.setState(ui.getInitialState());
  const multiplayer = (await import("../../../stores/multiplayerStore.ts")).useMultiplayerStore;
  multiplayer.setState({ activePlayerId: 0, isSpectator: false });
  const { setGameStoreForTest } = await import("../../../test/helpers/gameStoreHelpers.ts");
  function seed(objects: GameObject[], overrides: Partial<GameState> = {}) {
    const state = gameStateFactory.withPlayers(0, 1).withObjects(...objects).build(overrides);
    setGameStoreForTest({ gameState: state, gameMode: "online",
      legalActions: [], legalActionsByObject: {}, spellCosts: {}, viewerInteraction: null });
    return state;
  }
  activeFixture = { preferences, store, ui, seed, admit: () => admit(json(catalog)), reset() {
    store.setState(store.getInitialState());
    ui.setState(ui.getInitialState());
    preferences.setState(preferences.getInitialState());
    connectivity.setState(connectivity.getInitialState());
    multiplayer.setState(multiplayer.getInitialState());
  } };
  return activeFixture;
}

async function expectImage(container: HTMLElement, url: string) {
  await waitFor(() => expect(container.querySelector(`img[src="${url}"]`)).not.toBeNull());
}

async function toggleCycle(env: Fixture, container: HTMLElement, ownerUrl = URL_A, viewerUrl = URL_V) {
  await expectImage(container, ownerUrl);
  act(() => env.preferences.getState().setShowOwnerPrintings(false));
  await expectImage(container, viewerUrl);
  act(() => env.preferences.getState().setShowOwnerPrintings(true));
  await expectImage(container, ownerUrl);
}

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.resetModules();
  vi.doUnmock("../../../services/platform.ts");
  dispatch.mockClear();
  localStorage.clear();
});
afterEach(() => {
  cleanup();
  act(() => activeFixture?.reset());
  activeFixture = undefined;
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  vi.useRealTimers();
  vi.resetModules();
  localStorage.clear();
});

describe("owner printing carrier surfaces (group 1)", () => {
  it("V1 admits Scry art without gating normal art and follows the real Settings checkbox", async () => {
    const env = await setup({ holdCatalog: true });
    const object = card({ zone: "Library", owner: 1, controller: 0 });
    env.seed([object], { waiting_for: { type: "ScryChoice", data: { player: 0, cards: [object.id] } },
      derived: { unique_authorized_submitter: 0 } });
    const { CardChoiceModal } = await import("../../modal/CardChoiceModal.tsx");
    const { PreferencesModal } = await import("../../settings/PreferencesModal.tsx");
    const surface = render(<CardChoiceModal />);
    await expectImage(surface.container, URL_V);
    await act(async () => env.admit());
    await expectImage(surface.container, URL_A);
    const settings = render(<PreferencesModal onClose={vi.fn()} initialTab="visual" />);
    const checkbox = screen.getByRole("checkbox", { name: "Show cards as their owners chose" });
    fireEvent.click(checkbox);
    await expectImage(surface.container, URL_V);
    fireEvent.click(checkbox);
    await expectImage(surface.container, URL_A);
    settings.unmount();
    for (const pair of [printingB, undefined, null, foreignPrinting, printingA]) {
      const current = env.store.getState().gameState!;
      act(() => env.store.setState({ gameState: { ...current,
        objects: { ...current.objects, [object.id]: { ...object, printing: pair } },
      } }));
      await expectImage(surface.container, pair === printingB ? URL_B : pair === printingA ? URL_A : URL_V);
    }
    fireEvent.click(within(surface.container).getByRole("button", { name: "Top" }));
    fireEvent.click(within(surface.container).getByRole("button", { name: /Confirm/i }));
    expect(dispatch).toHaveBeenCalledWith({ type: "SelectCards", data: { cards: [] } });
    env.seed([object], { waiting_for: { type: "ScryChoice", data: { player: 1, cards: [object.id] } },
      derived: { unique_authorized_submitter: 1 } });
    expect(within(surface.container).queryByRole("button", { name: /Confirm/i })).toBeNull();
  });

  it.each(["full", "ghost", "crop", "attachment", "scheme", "plane"] as const)(
    "V2 forwards the selected board carrier through %s", async (kind) => {
      const env = await setup();
      env.preferences.setState({ battlefieldCardDisplay: "full_card" });
      const object = card({ owner: 1, controller: 0, zone: kind === "ghost" ? "Exile" : "Battlefield" });
      const host = card({ printing: null });
      const overrides: Partial<GameState> = {
        derived: {
          linked_exile_ids: { [host.id]: [object.id] },
          archenemy: { archenemy: 0, scheme_deck_count: 1, active_scheme_ids: [object.id] },
          planechase: { active_plane: object.id, planar_deck_count: 1, can_roll: false,
            current_roll_cost: { type: "NoCost" } },
        },
      };
      env.seed(kind === "ghost" || kind === "attachment" ? [host, object] : [object], overrides);
      const { PermanentCard } = await import("../../board/PermanentCard.tsx");
      const { ArtCropCard } = await import("../ArtCropCard.tsx");
      const { AttachmentFan } = await import("../../board/AttachmentFan.tsx");
      const { ArchenemyPanel } = await import("../../board/ArchenemyPanel.tsx");
      const { PlanechasePanel } = await import("../../board/PlanechasePanel.tsx");
      if (kind === "attachment") {
        env.ui.setState({ attachmentFanHostId: host.id });
        env.store.setState({ viewerInteraction: {
          waitingForKind: { simultaneous: null, terminal: false, code: "choose" },
          authorizedSubmitters: [0], canSubmit: false, autoPassRecommended: false,
          opportunities: [], attachmentFans: {}, availability: { type: "waiting" },
          attachmentViews: { [host.id]: { hostId: host.id, cards: [{ objectId: object.id, submission: null }] } },
        } });
      }
      const surface = render(kind === "crop" ? <ArtCropCard objectId={object.id} />
        : kind === "attachment" ? <AttachmentFan />
        : kind === "scheme" ? <ArchenemyPanel />
        : kind === "plane" ? <PlanechasePanel />
        : <PermanentCard objectId={kind === "ghost" ? host.id : object.id} />);
      const container = kind === "ghost"
        ? surface.container.querySelector<HTMLElement>(`[data-exile-ghost="${object.id}"]`)!
        : kind === "attachment" ? document.body.querySelector<HTMLElement>(`[data-object-id="${object.id}"]`)! : surface.container;
      expect(container).not.toBeNull();
      await toggleCycle(env, container, kind === "crop" ? CROP_A : URL_A, kind === "crop" ? CROP_V : URL_V);
      const current = env.store.getState().gameState!;
      act(() => env.store.setState({ gameState: { ...current,
        objects: { ...current.objects, [object.id]: { ...object, printing: undefined } },
      } }));
      await expectImage(container, kind === "crop" ? CROP_V : URL_V);
    },
  );

  it("V2 preserves a concealed board back beside a public positive card", async () => {
    const env = await setup();
    env.preferences.setState({ battlefieldCardDisplay: "full_card" });
    const visible = card();
    const hidden = card({ name: "", printed_ref: null, printing: undefined, face_down: true,
      display_visible_to_viewer: false });
    env.seed([visible, hidden]);
    const { PermanentCard } = await import("../../board/PermanentCard.tsx");
    const surface = render(<><PermanentCard objectId={visible.id} /><PermanentCard objectId={hidden.id} /></>);
    await toggleCycle(env, surface.container);
    expect(within(surface.container).getByRole("img", { name: "Card back" })).toBeInTheDocument();
  });

  it.each(["hand", "Exile", "Graveyard"] as const)(
    "V3 retains same-name printing identities through the memoized %s fan", async (kind) => {
      const env = await setup();
      const zone = kind === "hand" ? "Hand" : kind;
      const a = card({ zone, owner: 0, controller: 1 });
      const b = card({ zone, printing: printingB });
      const state: Partial<GameState> = {
        players: gameStateFactory.withPlayers({ id: 0, hand: kind === "hand" ? [a.id, b.id] : [],
          graveyard: kind === "Graveyard" ? [a.id, b.id] : [] }, 1).build().players,
        exile: kind === "Exile" ? [a.id, b.id] : [],
      };
      env.seed([a, b], state);
      act(() => env.store.setState({ legalActionsByObject: {
        [a.id]: [{ type: "CastSpell", data: { object_id: a.id, card_id: a.card_id, targets: [] } }],
        [b.id]: [{ type: "CastSpell", data: { object_id: b.id, card_id: b.card_id, targets: [] } }],
      } }));
      const { PlayerHand } = await import("../../hand/PlayerHand.tsx");
      const surface = render(<PlayerHand />);
      const target = (id: number) => surface.container.querySelector<HTMLElement>(`[data-object-id="${id}"]`)!;
      await expectImage(target(a.id), URL_A);
      await expectImage(target(b.id), URL_B);
      await toggleCycle(env, target(a.id));
      await expectImage(target(b.id), URL_B);
      const current = env.store.getState().gameState!;
      act(() => env.store.setState({ gameState: { ...current,
        objects: { ...current.objects, [a.id]: { ...a, printing: printingB }, [b.id]: { ...b, printing: null } },
        players: current.players.map((player) => player.id === 0 ? { ...player,
          hand: [...player.hand].reverse(), graveyard: [...player.graveyard].reverse() } : player),
        exile: [...current.exile].reverse(),
      } }));
      await expectImage(target(a.id), URL_B);
      await expectImage(target(b.id), URL_V);
    },
  );

  it.each(["drawer", "held", "opponent"] as const)(
    "V3 forwards the visible object on the %s hand surface", async (kind) => {
      const env = await setup();
      const object = card({ zone: "Hand", owner: kind === "opponent" ? 1 : 0,
        controller: kind === "opponent" ? 1 : 0 });
      const state: Partial<GameState> = { players: gameStateFactory.withPlayers(
        { id: 0, hand: kind === "opponent" ? [] : [object.id] },
        { id: 1, hand: kind === "opponent" ? [object.id] : [] },
      ).build().players };
      env.seed([object], state);
      env.ui.setState({ mobileHandOpen: true });
      const { MobileHandDrawer } = await import("../../hand/MobileHandDrawer.tsx");
      const { MobileHeldHandCard } = await import("../../hand/MobileHeldHandCard.tsx");
      const { OpponentHand } = await import("../../hand/OpponentHand.tsx");
      const held = (value: GameObject) => <MobileHeldHandCard object={value} gesture={{
        objectId: value.id, phase: "drag", offsetX: 0, offsetY: 0, playable: false, castReady: false,
        sourceOrigin: { bottom: 748, centerX: 280, height: 168, rotation: 0, top: 580, width: 120 },
      }} />;
      const surface = render(kind === "drawer" ? <MobileHandDrawer />
        : kind === "held" ? held(object) : <OpponentHand playerId={1} />);
      const container = kind === "opponent" ? surface.container : document.body;
      await toggleCycle(env, container);
      if (kind === "held") surface.rerender(held({ ...object, printing: undefined }));
      else env.seed([{ ...object, printing: undefined }], state);
      await expectImage(container, URL_V);
    },
  );

  it("V3 leaves a hidden opponent card backed while its projected sibling shows owner art", async () => {
    const env = await setup();
    const visible = card({ zone: "Hand", owner: 1, controller: 1 });
    const hidden = card({ zone: "Hand", owner: 1, controller: 1, name: "", printing: undefined,
      printed_ref: null, display_visible_to_viewer: false });
    env.seed([visible, hidden], { players: gameStateFactory.withPlayers(0,
      { id: 1, hand: [visible.id, hidden.id] }).build().players });
    const { OpponentHand } = await import("../../hand/OpponentHand.tsx");
    const surface = render(<OpponentHand playerId={1} />);
    await toggleCycle(env, surface.container);
    expect(within(surface.container).getByRole("img", { name: "Card back" })).toBeInTheDocument();
  });

  it.each(["peek", "attackers", "attachment"] as const)(
    "V4 forwards the selected HUD object through %s", async (kind) => {
      const env = await setup();
      const object = card({ owner: 0, controller: 1 });
      env.seed([object]);
      const { BattlefieldPeekPopover } = await import("../../hud/BattlefieldPeekPopover.tsx");
      const { IncomingAttackersPopover } = await import("../../hud/IncomingAttackersPopover.tsx");
      const { DialogAttachmentCard } = await import("../../hud/DialogAttachmentCard.tsx");
      const surface = render(kind === "peek" ? <BattlefieldPeekPopover playerId={1}
        opponentName="Opponent" seatColor="#fff" isTargeting={false} legalTargetIds={[]} />
        : kind === "attackers" ? <IncomingAttackersPopover attackerIds={[object.id]} opponentName="Opponent" />
        : <DialogAttachmentCard objectId={object.id} widthPx={120} />);
      await toggleCycle(env, surface.container);
      env.seed([{ ...object, printing: null }]);
      await expectImage(surface.container, URL_V);
      env.seed([]);
      expect(surface.container.querySelector("img")).toBeNull();
    },
  );

  it.each(["graveyard", "library", "viewer", "dock", "commander", "signature"] as const)(
    "V5 forwards the visible %s zone authority", async (kind) => {
      const env = await setup();
      env.preferences.setState({ commandZoneDisplay: "compact" });
      const object = card({
        zone: kind === "library" ? "Library" : kind === "graveyard" || kind === "viewer" ? "Graveyard" : "Command",
        is_commander: kind === "commander" || kind === "dock",
        signature_spell: kind === "signature" ? {} : undefined,
      });
      const state: Partial<GameState> = {
        players: gameStateFactory.withPlayers({ id: 0, library: [object.id], graveyard: [object.id] }, 1).build().players,
        command_zone: [object.id],
      };
      env.seed([object], state);
      const { GraveyardPile } = await import("../../zone/GraveyardPile.tsx");
      const { LibraryPile } = await import("../../zone/LibraryPile.tsx");
      const { ZoneViewer } = await import("../../zone/ZoneViewer.tsx");
      const { CommandDock } = await import("../../zone/CommandDock.tsx");
      const { CommanderCardZone } = await import("../../zone/CommanderCardZone.tsx");
      const surface = render(kind === "graveyard" ? <GraveyardPile playerId={0} onClick={vi.fn()} />
        : kind === "library" ? <LibraryPile playerId={0} />
        : kind === "viewer" ? <ZoneViewer playerId={0} zone="graveyard" onClose={vi.fn()} />
        : kind === "dock" ? <CommandDock playerId={0} isMirrored={false} />
        : <CommanderCardZone playerId={0} />);
      const container = kind === "viewer" ? document.body : surface.container;
      await toggleCycle(env, container);
      env.seed([{ ...object, printing: undefined }], state);
      await expectImage(container, URL_V);
    },
  );

  it.each(["departed", "conflicting-live", "null", "omitted", "legacy", "foreign"] as const)(
    "V6 respects present stack details for %s source printing", async (kind) => {
      const env = await setup();
      const source = card({ zone: "Stack", printing: printingB });
      const entry = buildStackEntry({ source_id: source.id });
      env.seed(kind === "departed" ? [] : [source], { stack: [entry] });
      const details = kind === "legacy" ? undefined : { source_name: "Alpha", kind_label: "Spell",
        ...(kind === "omitted" ? {} : { printing: kind === "null" ? null
          : kind === "foreign" ? foreignPrinting : printingA }) };
      const { StackEntry } = await import("../../stack/StackEntry.tsx");
      const surface = render(<StackEntry entry={entry}
        details={{ source_name: "Alpha", kind_label: "Spell", printing: printingA }} index={0} isTop
        cardSize={{ width: 120, height: 168 }} />);
      await expectImage(surface.container, URL_A);
      surface.rerender(<StackEntry entry={entry} details={details} index={0} isTop
        cardSize={{ width: 120, height: 168 }} />);
      const expected = kind === "legacy" ? URL_B
        : kind === "null" || kind === "omitted" || kind === "foreign" ? URL_V : URL_A;
      await toggleCycle(env, surface.container, expected);
    },
  );

  it("V7 uses emblem-source printing rather than the emblem or another live object", async () => {
    const env = await setup();
    const unrelated = card({ printing: printingB });
    const emblem = card({ zone: "Command", name: "Emblem", printed_ref: null, is_emblem: true,
      printing: printingB, emblem_source: { name: "Alpha", printing: printingA,
        printed_ref: { oracle_id: ORACLE, face_name: "Alpha" } } });
    const state = { command_zone: [emblem.id] };
    env.seed([emblem, unrelated], state);
    const { CommandZone } = await import("../../zone/CommandZone.tsx");
    const surface = render(<CommandZone playerId={0} />);
    await toggleCycle(env, surface.container, CROP_A, CROP_V);
    env.seed([{ ...emblem, emblem_source: { ...emblem.emblem_source!, printing: undefined } }, unrelated], state);
    await expectImage(surface.container, CROP_V);
  });

  it("V2 keeps synthesized token art independent of an absent owner printing", async () => {
    const env = await setup();
    env.preferences.setState({ battlefieldCardDisplay: "full_card" });
    const visible = card();
    const token = card({ name: "Spirit", printed_ref: null, printing: undefined, display_source: "Token",
      token_image_ref: { scryfall_id: OWNER_B, scryfall_oracle_id: OTHER_ORACLE, face_name: "Spirit", preset_id: "" } });
    env.seed([visible, token]);
    const { PermanentCard } = await import("../../board/PermanentCard.tsx");
    const surface = render(<><PermanentCard objectId={visible.id} /><PermanentCard objectId={token.id} /></>);
    await toggleCycle(env, surface.container);
    await expectImage(surface.container, URL_B);
  });

  it("V5 keeps a hidden library top backed beside an authorized public surface", async () => {
    const env = await setup();
    const visible = card({ zone: "Graveyard" });
    const hidden = card({ zone: "Library", name: "", printing: undefined, printed_ref: null,
      display_visible_to_viewer: false });
    env.seed([visible, hidden], { players: gameStateFactory.withPlayers(
      { id: 0, graveyard: [visible.id], library: [hidden.id] }, 1).build().players });
    const { GraveyardPile } = await import("../../zone/GraveyardPile.tsx");
    const { LibraryPile } = await import("../../zone/LibraryPile.tsx");
    const surface = render(<><GraveyardPile playerId={0} onClick={vi.fn()} /><LibraryPile playerId={0} /></>);
    await toggleCycle(env, surface.container);
    expect(within(surface.container).getByRole("img", { name: "Card back" })).toBeInTheDocument();
  });
  it.each([undefined, null, foreignPrinting])(
    "V14 rejects absent or foreign printing %j without changing normal identity", async (pair) => {
      const env = await setup();
      const { CardImage } = await import("../CardImage.tsx");
      const surface = render(<CardImage cardName="Alpha" oracleId={ORACLE} ownerPrinting={printingA} />);
      await toggleCycle(env, surface.container);
      surface.rerender(<CardImage cardName="Alpha" oracleId={ORACLE} ownerPrinting={pair} />);
      await expectImage(surface.container, URL_V);
    },
  );
});
