import { act, cleanup, render, renderHook, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { CardPrinting } from "../../adapter/types.ts";
import type { SupportedLng } from "../../i18n/resources.ts";
import type { PrintingEntry } from "../../services/scryfall.ts";
import type { ArtChainEntry } from "../../stores/preferencesStore.ts";
import type { VisualPackBackend } from "../../services/visualPacks/backend.ts";
import { cardCandidateGroups, semanticCardCandidateGroups } from "../../services/visualPacks/candidateKeys.ts";
import { assetKey, catalogRoot, installedRevision, packId } from "../../services/visualPacks/types.ts";
import type { CandidateKey, ResolvedAsset, ResolutionKey } from "../../services/visualPacks/types.ts";

const A = "11111111-1111-4111-8111-111111111111";
const B = "22222222-2222-4222-8222-222222222222";
const ids = {
  owner: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa1",
  owner2: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa2",
  viewer: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa3",
  chain: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa4",
  source: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa5",
  foreign: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbb1",
  list: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa6",
};
function printing(id: string, set: string, number: string): PrintingEntry {
  return {
    id, set, collector_number: number, set_name: set, released_at: "2024-01-01",
    border_color: "black", frame_effects: [], full_art: false,
    faces: [0, 1].map((face) => ({
      normal: `https://img.example/${id}/${face}.jpg`,
      small: `https://img.example/${id}/${face}-small.jpg`,
      art_crop: `https://img.example/${id}/${face}-crop.jpg`,
    })),
  };
}
const owner = printing(ids.owner, "own", "01");
const owner2 = printing(ids.owner2, "own", "2");
const viewer = printing(ids.viewer, "pin", "3");
const chain = printing(ids.chain, "chn", "4");
const source = printing(ids.source, "src", "5");
const foreign = printing(ids.foreign, "bad", "9");
const list = printing(ids.list, "plst", "6");
const catalog = { [A]: [owner, owner2, viewer, chain, source, list], [B]: [foreign] };
const ownerPair: CardPrinting = { set_code: "OWN", collector_number: "01" };
const owner2Pair: CardPrinting = { set_code: "own", collector_number: "2" };
const sourcePair = { setCode: "SRC", collectorNumber: "5" };
const chainPreference: ArtChainEntry[] = [{ type: "set", setCode: "chn", label: "Chain" }];
const legacyCases: Array<[string, { pin?: string | null; artChain?: ArtChainEntry[] }, string | null | undefined]> = [
  ["pin over chain", {}, viewer.faces[0].normal],
  ["chain over source", { pin: null }, chain.faces[0].normal],
  ["empty chain source", { pin: null, artChain: [] }, source.faces[0].normal],
  ["stale pin empty chain", { pin: "missing", artChain: [] }, source.faces[0].normal],
  ["stale pin nonempty chain", { pin: "missing" }, `https://img.example/${A}/canonical.jpg`],
  ["explicit picker", {}, owner2.faces[0].normal],
];
function jsonResponse(value: unknown): Response {
  return new Response(JSON.stringify(value), { status: 200, headers: { "Content-Type": "application/json" } });
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}
function metadata() {
  return Object.fromEntries([[A, "Owner Card"], [B, "Foreign Card"]].flatMap(([oracle, name]) => {
    const entry = {
      oracle_id: oracle, name, face_names: [name.toLowerCase(), "back card"], layout: "transform",
      faces: [{ normal: `https://img.example/${oracle}/canonical.jpg` }, { normal: `https://img.example/${oracle}/back.jpg`, orientation: "landscape" }],
      mana_cost: "", cmc: 0, type_line: "Creature", colors: [], color_identity: [], keywords: [],
    };
    return [[name.toLowerCase(), entry], [oracle, entry]];
  }));
}
async function setup(options: {
  installed?: PrintingEntry[];
  canonical?: boolean;
  nameSemantic?: PrintingEntry;
  printings?: unknown;
  data?: Promise<Response>;
  offline?: boolean;
  language?: SupportedLng;
  pin?: string | null;
  artChain?: ArtChainEntry[];
  cold?: boolean;
} = {}) {
  const installed = new Map<CandidateKey, ResolvedAsset[]>();
  const backendResolve = vi.fn(async (keys: ResolutionKey[]) => ({
    revision: installedRevision("0"),
    entries: keys.map((key, ordinal) => ({ ordinal, key, matches: key.kind === "candidate" ? installed.get(key.key) ?? [] : [] })),
  }));
  const backend = { resolve: backendResolve, subscribeRevision: async () => () => {} } as unknown as VisualPackBackend;
  // Dynamic imports intentionally exercise fresh module/cache boundaries after
  // vi.resetModules; static imports would retain another test's stores.
  const actual = await import("../../services/visualPacks/repository.ts");
  const repository = new actual.VisualPackRepository(async () => backend);
  const realResolve = repository.resolve.bind(repository);
  const resolve = vi.spyOn(repository, "resolve");
  vi.doMock("../../services/visualPacks/repository.ts", () => ({ ...actual, visualPackRepository: repository }));
  function addInstalled(entry: PrintingEntry, oracle = entry === foreign ? B : A, canonical = false) {
    for (const faceIndex of [0, 1]) for (const size of ["small", "normal", "art_crop"] as const) {
      const variant = size === "art_crop" ? "art_crop" : "full_card";
      const groups = cardCandidateGroups({ englishPrintingId: entry.id, oracleId: canonical ? oracle : undefined, englishAliases: canonical ? ["Owner Card", "Back Card"] : undefined, faceIndex, variant, rung: size });
      const name = entry === foreign ? "Foreign Card" : "Owner Card";
      groups.push(...semanticCardCandidateGroups({
        sourceSetCode: entry.set, sourceCollectorNumber: entry.collector_number,
        cardName: name, faceName: faceIndex ? "Back Card" : name, variant, rung: size,
      }).slice(0, 1));
      if (canonical) groups.push(...semanticCardCandidateGroups({ oracleId: oracle, cardName: "Owner Card", faceName: faceIndex ? "Back Card" : "Owner Card", variant, rung: size }));
      const match: ResolvedAsset = { packId: packId("deck_library"), assetKey: assetKey(`asset:v1:exact_printing:${entry.id}_${faceIndex}`), catalogRoot: catalogRoot("a".repeat(64)), url: `/installed/${entry.id}/${faceIndex}-${size}.jpg`, media: "image/jpeg" };
      for (const group of groups) for (const key of group.keys) installed.set(key, [match]);
    }
  }
  for (const entry of options.installed ?? []) addInstalled(entry);
  if (options.canonical) addInstalled(viewer, A, true);
  if (options.nameSemantic) {
    const entry = options.nameSemantic;
    for (const faceIndex of [0, 1]) for (const size of ["small", "normal", "art_crop"] as const) {
      const groups = semanticCardCandidateGroups({
        cardName: "Owner Card", faceName: faceIndex ? "Back Card" : "Owner Card",
        variant: size === "art_crop" ? "art_crop" : "full_card", rung: size,
      });
      const match: ResolvedAsset = {
        packId: packId("deck_library"), assetKey: assetKey(`asset:v1:exact_printing:${entry.id}_${faceIndex}`),
        catalogRoot: catalogRoot("a".repeat(64)), url: `/installed/${entry.id}/${faceIndex}-${size}.jpg`, media: "image/jpeg",
      };
      for (const group of groups) for (const key of group.keys) installed.set(key, [match]);
    }
  }
  const fetchMock = vi.fn((url: string) => {
    if (url === "/scryfall-data.json") return options.data ?? Promise.resolve(jsonResponse(metadata()));
    if (url === "/scryfall-printings.json") return options.printings instanceof Promise ? options.printings : Promise.resolve(jsonResponse(options.printings ?? catalog));
    return Promise.resolve(jsonResponse({}));
  });
  vi.stubGlobal("fetch", fetchMock);
  const { usePreferencesStore } = await import("../../stores/preferencesStore.ts");
  usePreferencesStore.setState({ language: options.language ?? "en", showOwnerPrintings: true, artOverrides: {}, artChain: options.artChain ?? chainPreference });
  if (options.pin !== null) usePreferencesStore.getState().setArtOverride(A, { scryfallId: options.pin ?? viewer.id, setCode: "pin", collectorNumber: "3" });
  const { useConnectivityStore } = await import("../../stores/connectivityStore.ts");
  useConnectivityStore.setState({ forcedOffline: options.offline ?? false, browserOnline: true });
  const scryfall = await import("../../services/scryfall.ts");
  if (!options.cold) await scryfall.loadScryfallData();
  const { useCardImage, BoundedCache } = await import("../useCardImage.ts");
  return { useCardImage, BoundedCache, resolve, realResolve, fetchMock, prefs: usePreferencesStore, scryfall, connectivity: useConnectivityStore };
}

beforeEach(() => {
  vi.resetModules();
  vi.restoreAllMocks();
  vi.doUnmock("../../services/scryfall.ts");
  vi.doUnmock("../../services/visualPacks/repository.ts");
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.doUnmock("../../services/visualPacks/repository.ts");
});

describe("legacy reference measurements", () => {
  it.each(legacyCases)("%s", async (label, preferences, expected) => {
    const { useCardImage } = await setup(preferences);
    const opts = { oracleId: A, sourcePrinting: sourcePair, ...(label === "explicit picker" ? { scryfallId: owner2.id } : {}) };
    const first = renderHook(() => useCardImage("Owner Card", opts));
    await waitFor(() => expect(first.result.current.src).toBe(expected));
    expect(first.result.current.isLoading).toBe(false);
    const sibling = renderHook(() => useCardImage("Owner Card", { ...opts }));
    expect(sibling.result.current.src).toBe(expected);
  });
  it("offline installed order", async () => {
    const { useCardImage } = await setup({ installed: [viewer, owner], canonical: true, offline: true });
    const { result } = renderHook(() => useCardImage("Owner Card", { oracleId: A, sourcePrinting: sourcePair }));
    await waitFor(() => expect(result.current.src).toBe(`/installed/${viewer.id}/0-normal.jpg`));
    expect(result.current.source?.kind).toBe("installed");
  });
  it("token request without owner", async () => {
    const { useCardImage } = await setup({ installed: [viewer], offline: true });
    const { result } = renderHook(() => useCardImage("Owner Card", {
      isToken: true, tokenImageRef: { scryfall_id: viewer.id, scryfall_oracle_id: A, face_name: "Owner Card", preset_id: "" },
    }));
    await waitFor(() => expect(result.current.src).toBe(`/installed/${viewer.id}/0-normal.jpg`));
    expect(result.current.isLoading).toBe(false);
  });
});

describe("owner printing admission and image selection", () => {
  it.each([
    ["normal", 0], ["art_crop", 0], ["normal", 1],
  ] as const)("V1 owner installed member outranks viewer art (%s, face %s)", async (size, face) => {
    const pending = deferred<Response>();
    const { useCardImage, resolve, fetchMock } = await setup({
      installed: [owner, viewer, source, chain, foreign], canonical: true, printings: pending.promise,
    });
    const { result } = renderHook(() => useCardImage("Owner Card", {
      oracleId: A, faceName: face ? "Back Card" : "Owner Card", size,
      ownerPrinting: ownerPair, sourcePrinting: sourcePair, scryfallId: viewer.id,
    }));
    await waitFor(() => expect(fetchMock).toHaveBeenCalledWith("/scryfall-printings.json"));
    expect(resolve).not.toHaveBeenCalled();
    expect(result.current.src).toBeNull();
    expect(result.current.isLoading).toBe(true);
    await act(async () => pending.resolve(jsonResponse(catalog)));
    await waitFor(() => expect(result.current.src).toBe(`/installed/${owner.id}/${face}-${size}.jpg`));
    expect(result.current.source?.kind).toBe("installed");
    expect(result.current.isRotated).toBe(face === 1);
    expect(result.current.isLoading).toBe(false);
  });

  it.each(["en", "de", "zh"] as const)("V2 owner remote wins over unrelated installed hit (%s)", async (language) => {
    const { useCardImage } = await setup({ installed: [viewer, foreign], canonical: true, language });
    const observed: Array<string | null> = [];
    const { result, rerender } = renderHook(({ enabled }) => {
      const image = useCardImage("Owner Card", {
        oracleId: A, faceName: "Back Card", ownerPrinting: ownerPair, showOwnerPrintings: enabled,
      });
      observed.push(image.src);
      return image;
    }, { initialProps: { enabled: true } });
    await waitFor(() => expect(result.current.src).toBe(owner.faces[1].normal));
    expect(result.current.source?.kind).toBe("remote");
    expect(result.current.isRotated).toBe(true);
    expect(result.current.isLoading).toBe(false);
    expect(observed.filter((src) => src !== null).every((src) => src === owner.faces[1].normal)).toBe(true);
    rerender({ enabled: false });
    await waitFor(() => expect(result.current.src).toBe(`/installed/${viewer.id}/1-normal.jpg`));
    expect(result.current.source?.kind).toBe("installed");
  });

  it("V2 failed owner installed art advances only to the owner's remote image", async () => {
    const { useCardImage } = await setup({ installed: [owner, viewer], canonical: true });
    const { result } = renderHook(() => useCardImage("Owner Card", { oracleId: A, ownerPrinting: ownerPair }));
    const installed = `/installed/${owner.id}/0-normal.jpg`;
    await waitFor(() => expect(result.current.src).toBe(installed));
    act(() => result.current.advanceFailedSource?.(installed));
    await waitFor(() => expect(result.current.src).toBe(owner.faces[0].normal));
    expect(result.current.source?.kind).toBe("remote");
    expect(result.current.isLoading).toBe(false);
    act(() => result.current.advanceFailedSource?.(owner.faces[0].normal!));
    expect(result.current.src).toBeNull();
    expect(result.current.isLoading).toBe(false);
  });

  it.each([
    ["off", ownerPair, false],
    ["absent", undefined, true],
    ["null", null, true],
    ["foreign", { set_code: "bad", collector_number: "9" }, true],
    ["empty", { set_code: "", collector_number: "" }, true],
    ["malformed", { set_code: 123, collector_number: null }, true],
    ["exact collector string", { set_code: "own", collector_number: "1" }, true],
    ["excluded List", { set_code: "plst", collector_number: "6" }, true],
  ])("V3 inactive or rejected owner preserves warm legacy request (%s)", async (_label, pair, enabled) => {
    const { useCardImage, resolve, prefs, fetchMock } = await setup({ installed: [viewer, foreign, list], canonical: true });
    const baseline = renderHook(() => useCardImage("Owner Card", { oracleId: A, sourcePrinting: sourcePair }));
    const expected = `/installed/${viewer.id}/0-normal.jpg`;
    await waitFor(() => expect(baseline.result.current.src).toBe(expected));
    baseline.unmount();
    const options = { oracleId: A, sourcePrinting: sourcePair, ownerPrinting: pair as CardPrinting | null | undefined, showOwnerPrintings: enabled as boolean };
    const current = renderHook(() => useCardImage("Owner Card", options));
    await waitFor(() => expect(current.result.current.src).toBe(expected));
    expect(current.result.current.source?.kind).toBe("installed");
    if (["off", "absent", "null", "empty", "malformed"].includes(_label as string)) {
      expect(fetchMock).not.toHaveBeenCalledWith("/scryfall-printings.json");
      expect(current.result.current.isLoading).toBe(false);
    }
    const before = resolve.mock.calls.length;
    act(() => prefs.getState().setShowOwnerPrintings(false));
    expect(current.result.current.src).toBe(expected);
    expect(resolve).toHaveBeenCalledTimes(before);
    current.unmount();
    const warm = renderHook(() => useCardImage("Owner Card", options));
    // Rejected membership may need preparation; its image request stays legacy.
    await waitFor(() => expect(warm.result.current.src).toBe(expected));
    expect(warm.result.current.isLoading).toBe(false);
  });

  it.each([
    [A, "Foreign Card"], ["33333333-3333-4333-8333-333333333333", "Owner Card"],
  ])("V4 foreign printing never renders despite installed asset (%s)", async (initialOracle, cardName) => {
    const { useCardImage } = await setup({ installed: [viewer, foreign], canonical: true });
    const forged = { set_code: "BAD", collector_number: "9" };
    const { result, rerender } = renderHook(({ oracleId }) => useCardImage(cardName, {
      oracleId, ownerPrinting: forged,
    }), { initialProps: { oracleId: initialOracle } });
    await waitFor(() => expect(result.current.src).toBe(`/installed/${viewer.id}/0-normal.jpg`));
    rerender({ oracleId: B });
    expect(result.current.src).toBeNull();
    await waitFor(() => expect(result.current.src).toBe(`/installed/${foreign.id}/0-normal.jpg`));
    expect(result.current.source?.kind).toBe("installed");
  });

  it("V4 same-set collector and explicit owner precedence are exact", async () => {
    const { useCardImage } = await setup({ installed: [owner, owner2, viewer] });
    const { result, rerender } = renderHook(({ pair }) => useCardImage("Foreign Card", {
      oracleId: A, ownerPrinting: pair, scryfallId: viewer.id,
    }), { initialProps: { pair: ownerPair } });
    await waitFor(() => expect(result.current.src).toBe(`/installed/${owner.id}/0-normal.jpg`));
    rerender({ pair: owner2Pair });
    expect(result.current.src).toBeNull();
    await waitFor(() => expect(result.current.src).toBe(`/installed/${owner2.id}/0-normal.jpg`));
  });

  it("V5 rejected owner preserves a live installed-to-remote failure continuation", async () => {
    const { useCardImage, prefs, resolve } = await setup({ installed: [viewer] });
    const { result, rerender } = renderHook(({ pair }) => useCardImage("Owner Card", {
      oracleId: A, ownerPrinting: pair,
    }), { initialProps: { pair: ownerPair } });
    await waitFor(() => expect(result.current.src).toBe(owner.faces[0].normal));
    act(() => prefs.getState().setShowOwnerPrintings(false));
    const installed = `/installed/${viewer.id}/0-normal.jpg`;
    await waitFor(() => expect(result.current.src).toBe(installed));
    const before = resolve.mock.calls.length;
    rerender({ pair: { set_code: "xxx", collector_number: "none" } });
    act(() => prefs.getState().setShowOwnerPrintings(true));
    await waitFor(() => expect(result.current.src).toBe(installed));
    expect(resolve).toHaveBeenCalledTimes(before);
    act(() => result.current.advanceFailedSource?.(installed));
    await waitFor(() => expect(result.current.src).toBe(viewer.faces[0].normal));
    expect(result.current.isLoading).toBe(false);
  });

  it("V5 toggle and owner changes rekey mounted hook without unrelated churn", async () => {
    const { useCardImage, prefs, resolve } = await setup({ installed: [owner, owner2, viewer] });
    const { result, rerender, unmount } = renderHook(({ pair }) => useCardImage("Owner Card", {
      oracleId: A, ownerPrinting: { ...pair },
    }), { initialProps: { pair: ownerPair } });
    await waitFor(() => expect(result.current.src).toBe(`/installed/${owner.id}/0-normal.jpg`));
    let before = resolve.mock.calls.length;
    rerender({ pair: { ...ownerPair } });
    act(() => {
      prefs.getState().setArtOverride(B, { scryfallId: foreign.id, setCode: "bad", collectorNumber: "9" });
      prefs.setState({ showCardPreviewFooter: false });
    });
    expect(result.current.src).toBe(`/installed/${owner.id}/0-normal.jpg`);
    expect(resolve).toHaveBeenCalledTimes(before);
    act(() => prefs.getState().setShowOwnerPrintings(false));
    expect(result.current.src).toBeNull();
    await waitFor(() => expect(result.current.src).toBe(`/installed/${viewer.id}/0-normal.jpg`));
    act(() => prefs.getState().setShowOwnerPrintings(true));
    expect(result.current.src).not.toBe(`/installed/${viewer.id}/0-normal.jpg`);
    await waitFor(() => expect(result.current.src).toBe(`/installed/${owner.id}/0-normal.jpg`));
    before = resolve.mock.calls.length;
    rerender({ pair: owner2Pair });
    expect(result.current.src).toBeNull();
    await waitFor(() => expect(result.current.src).toBe(`/installed/${owner2.id}/0-normal.jpg`));
    expect(resolve.mock.calls.length).toBeGreaterThan(before);
    unmount();
    const warm = renderHook(() => useCardImage("Owner Card", { oracleId: A, ownerPrinting: { ...owner2Pair } }));
    expect(warm.result.current.src).toBe(`/installed/${owner2.id}/0-normal.jpg`);
    expect(warm.result.current.isLoading).toBe(false);
  });

  it("V5 inactive toggles and different rejected pairs do not redo legacy repository work", async () => {
    const { useCardImage, prefs, resolve } = await setup({ installed: [viewer] });
    const { result, rerender } = renderHook(({ pair }: { pair?: CardPrinting }) => useCardImage("Owner Card", {
      oracleId: A, ownerPrinting: pair,
    }), { initialProps: {} });
    const expected = `/installed/${viewer.id}/0-normal.jpg`;
    await waitFor(() => expect(result.current.src).toBe(expected));
    const before = resolve.mock.calls.length;
    act(() => prefs.getState().setShowOwnerPrintings(false));
    act(() => prefs.getState().setShowOwnerPrintings(true));
    expect(result.current.src).toBe(expected);
    expect(resolve).toHaveBeenCalledTimes(before);
    rerender({ pair: { set_code: "xxx", collector_number: "a" } });
    await waitFor(() => expect(result.current.src).toBe(expected));
    rerender({ pair: { set_code: "yyy", collector_number: "b" } });
    await waitFor(() => expect(result.current.src).toBe(expected));
    expect(resolve).toHaveBeenCalledTimes(before);
  });

  it("V6 late catalog after disabling cannot replace the viewer image", async () => {
    const pending = deferred<Response>();
    const { useCardImage, prefs, fetchMock } = await setup({ installed: [owner, viewer], printings: pending.promise });
    const { result } = renderHook(() => useCardImage("Owner Card", { oracleId: A, ownerPrinting: ownerPair }));
    await waitFor(() => expect(fetchMock).toHaveBeenCalledWith("/scryfall-printings.json"));
    act(() => prefs.getState().setShowOwnerPrintings(false));
    await waitFor(() => expect(result.current.src).toBe(`/installed/${viewer.id}/0-normal.jpg`));
    await act(async () => pending.resolve(jsonResponse(catalog)));
    expect(result.current.src).toBe(`/installed/${viewer.id}/0-normal.jpg`);
    expect(result.current.isLoading).toBe(false);
  });

  it.each(["oracle", "printing", "face"])("V6 late membership cannot overwrite the current %s selection", async (change) => {
    const pending = deferred<PrintingEntry[]>();
    const { useCardImage, scryfall } = await setup({ installed: [owner, owner2, foreign] });
    const get = vi.spyOn(scryfall, "getCardPrintings").mockImplementationOnce(() => pending.promise);
    const initial = { oracleId: A, pair: ownerPair, faceName: "Owner Card" };
    const { result, rerender } = renderHook(({ oracleId, pair, faceName }) => useCardImage("Owner Card", {
      oracleId, ownerPrinting: pair, faceName,
    }), { initialProps: initial });
    await waitFor(() => expect(get).toHaveBeenCalledWith(A));
    const next = change === "oracle" ? { oracleId: B, pair: { set_code: "bad", collector_number: "9" }, faceName: "Foreign Card" }
      : change === "printing" ? { ...initial, pair: owner2Pair }
        : { ...initial, faceName: "Back Card" };
    rerender(next);
    const entry = change === "oracle" ? foreign : change === "printing" ? owner2 : owner;
    const expected = `/installed/${entry.id}/${change === "face" ? 1 : 0}-normal.jpg`;
    await waitFor(() => expect(result.current.src).toBe(expected));
    await act(async () => pending.resolve([owner]));
    expect(result.current.src).toBe(expected);
    expect(result.current.isLoading).toBe(false);
  });

  it("V6 cold name lookup cannot publish after switching Oracle", async () => {
    const data = deferred<Response>();
    const { useCardImage, fetchMock } = await setup({ cold: true, data: data.promise, installed: [owner, foreign] });
    const { result, rerender } = renderHook(({ oracleId, pair }) => useCardImage("Owner Card", {
      oracleId, ownerPrinting: pair,
    }), { initialProps: { oracleId: undefined as string | undefined, pair: ownerPair } });
    await waitFor(() => expect(fetchMock).toHaveBeenCalledWith("/scryfall-data.json"));
    rerender({ oracleId: B, pair: { set_code: "bad", collector_number: "9" } });
    await act(async () => data.resolve(jsonResponse(metadata())));
    await waitFor(() => expect(result.current.src).toBe(`/installed/${foreign.id}/0-normal.jpg`));
    expect(result.current.isLoading).toBe(false);
  });

  it("V6 unmounted membership cannot poison a later warm presentation", async () => {
    const pending = deferred<PrintingEntry[]>();
    const { useCardImage, scryfall } = await setup({ installed: [owner, owner2, viewer] });
    const get = vi.spyOn(scryfall, "getCardPrintings").mockImplementationOnce(() => pending.promise);
    const old = renderHook(() => useCardImage("Owner Card", { oracleId: A, ownerPrinting: ownerPair }));
    await waitFor(() => expect(get).toHaveBeenCalledWith(A));
    old.unmount();
    const current = renderHook(() => useCardImage("Owner Card", { oracleId: A, ownerPrinting: owner2Pair }));
    await waitFor(() => expect(current.result.current.src).toBe(`/installed/${owner2.id}/0-normal.jpg`));
    await act(async () => pending.resolve([owner]));
    current.unmount();
    const warm = renderHook(() => useCardImage("Owner Card", { oracleId: A, ownerPrinting: owner2Pair }));
    expect(warm.result.current.src).toBe(`/installed/${owner2.id}/0-normal.jpg`);
  });

  it("V6 late repository completion cannot publish or cache the previous owner", async () => {
    const gate = deferred<void>();
    const { useCardImage, resolve, realResolve } = await setup({ installed: [owner, owner2] });
    let entered = false;
    resolve.mockImplementationOnce(async (request) => {
      const result = await realResolve(request);
      entered = true;
      await gate.promise;
      return result;
    });
    const { result, rerender } = renderHook(({ pair }) => useCardImage("Owner Card", {
      oracleId: A, ownerPrinting: pair,
    }), { initialProps: { pair: ownerPair } });
    await waitFor(() => expect(entered).toBe(true));
    rerender({ pair: owner2Pair });
    await waitFor(() => expect(result.current.src).toBe(`/installed/${owner2.id}/0-normal.jpg`));
    await act(async () => gate.resolve());
    expect(result.current.src).toBe(`/installed/${owner2.id}/0-normal.jpg`);
  });

  it.each(["failed", "empty Oracle", "offline", "excluded List"])("V7 unavailable membership terminates with legacy art (%s)", async (mode) => {
    const noCatalog = deferred<Response>();
    const { useCardImage, fetchMock } = await setup({
      installed: [owner, viewer, list], canonical: true, offline: mode === "offline",
      printings: mode === "empty Oracle" ? { [B]: [foreign] } : mode === "excluded List" ? catalog : noCatalog.promise,
    });
    const pair = mode === "excluded List" ? { set_code: "plst", collector_number: "6" } : ownerPair;
    const { result } = renderHook(() => useCardImage("Owner Card", { oracleId: A, ownerPrinting: pair }));
    if (mode === "failed" || mode === "offline") {
      await waitFor(() => expect(fetchMock).toHaveBeenCalledWith("/scryfall-printings.json"));
      await act(async () => noCatalog.resolve(new Response(null, { status: 503 })));
    }
    await waitFor(() => expect(result.current.src).toBe(`/installed/${viewer.id}/0-normal.jpg`));
    expect(result.current.isLoading).toBe(false);
    const calls = fetchMock.mock.calls.length;
    await act(async () => { await Promise.resolve(); });
    expect(fetchMock).toHaveBeenCalledTimes(calls);
  });

  it.each([true, false])("V7 admitted member without remote image settles (installed=%s)", async (installed) => {
    const noImage = { ...owner, faces: [{}] };
    const { useCardImage } = await setup({
      installed: installed ? [owner, viewer] : [viewer], canonical: true,
      printings: { ...catalog, [A]: [noImage, viewer] },
    });
    const { result } = renderHook(() => useCardImage("Owner Card", { oracleId: A, ownerPrinting: ownerPair }));
    await waitFor(() => expect(result.current.isLoading).toBe(false));
    expect(result.current.src).toBe(installed ? `/installed/${owner.id}/0-normal.jpg` : null);
    expect(result.current.source?.kind).toBe(installed ? "installed" : "fallback");
  });

  it("valid current-Oracle owner is not excluded by token rendering options", async () => {
    const { useCardImage } = await setup({ installed: [owner, viewer] });
    const { result } = renderHook(() => useCardImage("Owner Card", {
      oracleId: A, ownerPrinting: ownerPair, isToken: true, size: "art_crop",
    }));
    await waitFor(() => expect(result.current.src).toBe(`/installed/${owner.id}/0-art_crop.jpg`));
    expect(result.current.isLoading).toBe(false);
  });
});

describe("DOM hook-consumer smoke", () => {
  it.each([true, false])("switches owner → viewer → owner → next owner (installed=%s)", async (installed) => {
    const { useCardImage, prefs } = await setup({ installed: installed ? [owner, owner2, viewer] : [viewer], canonical: true });
    function Consumer({ pair }: { pair: CardPrinting }) {
      const image = useCardImage("Owner Card", { oracleId: A, ownerPrinting: pair });
      return <img alt="printing smoke" src={image.src ?? undefined} data-loading={String(image.isLoading)} />;
    }
    const { rerender } = render(<Consumer pair={ownerPair} />);
    const image = screen.getByRole("img", { name: "printing smoke" });
    const expectedOwner = installed ? `/installed/${owner.id}/0-normal.jpg` : owner.faces[0].normal;
    const expectedNext = installed ? `/installed/${owner2.id}/0-normal.jpg` : owner2.faces[0].normal;
    await waitFor(() => expect(image.getAttribute("src")).toBe(expectedOwner));
    act(() => prefs.getState().setShowOwnerPrintings(false));
    expect(image.getAttribute("src")).toBeNull();
    await waitFor(() => expect(image.getAttribute("src")).toBe(`/installed/${viewer.id}/0-normal.jpg`));
    act(() => prefs.getState().setShowOwnerPrintings(true));
    await waitFor(() => expect(image.getAttribute("src")).toBe(expectedOwner));
    rerender(<Consumer pair={owner2Pair} />);
    expect(image.getAttribute("src")).toBeNull();
    await waitFor(() => expect(image.getAttribute("src")).toBe(expectedNext));
    expect(image.getAttribute("data-loading")).toBe("false");
  });
});

describe("V3 remote parity", () => {
  const inactive: Array<{ mode: string; pair?: CardPrinting | null; enabled?: boolean }> = [
    { mode: "off", pair: ownerPair, enabled: false },
    { mode: "null", pair: null },
    { mode: "nonmember", pair: { set_code: "bad", collector_number: "9" } },
    { mode: "empty", pair: { set_code: "", collector_number: "" } },
    { mode: "malformed", pair: { set_code: 123, collector_number: null } as unknown as CardPrinting },
    { mode: "List", pair: { set_code: "plst", collector_number: "6" } },
  ];
  it.each(inactive.flatMap((input) => legacyCases.map(([label, preferences, expected]) => ({
    ...input, label, preferences, expected,
  }))))("$mode: $label", async ({ pair, enabled, label, preferences, expected }) => {
    const { useCardImage } = await setup(preferences);
    const options = {
      oracleId: A, sourcePrinting: sourcePair, ownerPrinting: pair, showOwnerPrintings: enabled,
      ...(label === "explicit picker" ? { scryfallId: owner2.id } : {}),
    };
    const { result } = renderHook(() => useCardImage("Owner Card", options));
    await waitFor(() => expect(result.current.src).toBe(expected));
    expect(result.current.source?.kind).toBe("remote");
    expect(result.current.isLoading).toBe(false);
    const warm = renderHook(() => useCardImage("Owner Card", { ...options }));
    expect(warm.result.current.src).toBe(expected);
    expect(warm.result.current.isLoading).toBe(false);
  });

  it("explicit toggle options override the preference in both directions", async () => {
    const { useCardImage, prefs } = await setup({ installed: [owner, viewer] });
    prefs.getState().setShowOwnerPrintings(false);
    const admitted = renderHook(() => useCardImage("Owner Card", { oracleId: A, ownerPrinting: ownerPair, showOwnerPrintings: true }));
    const disabled = renderHook(() => useCardImage("Owner Card", { oracleId: A, ownerPrinting: ownerPair, showOwnerPrintings: false }));
    await waitFor(() => expect(admitted.result.current.src).toBe(`/installed/${owner.id}/0-normal.jpg`));
    await waitFor(() => expect(disabled.result.current.src).toBe(`/installed/${viewer.id}/0-normal.jpg`));
  });
});

describe("V3 isolated cold-offline legacy identity", () => {
  async function isolatedRun(kind: "explicit" | "name", ownerPrinting?: CardPrinting, warmMetadata = false) {
    // Reference and candidate must not share metadata, preferences, or hook
    // caches. Sharing setup's warmed Scryfall singleton hides this regression.
    cleanup();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    vi.resetModules();
    vi.doUnmock("../../services/visualPacks/repository.ts");
    const { useCardImage, BoundedCache, resolve, fetchMock } = await setup({
      cold: !warmMetadata, offline: true, installed: [viewer, owner, list],
      canonical: true, nameSemantic: source,
    });
    // Observe real cache writes through the existing exported cache primitive;
    // no private-key export or test-only hook instrumentation is needed.
    const cacheWrites = vi.spyOn(BoundedCache.prototype, "set");
    const options = {
      ...(kind === "explicit" ? { oracleId: A, faceName: "Back Card", faceIndex: 0 } : {}),
      ownerPrinting,
    };
    function Consumer() {
      const image = useCardImage("Owner Card", { ...options });
      return <img alt="offline printing" src={image.src ?? undefined} data-loading={String(image.isLoading)} />;
    }
    const mounted = render(<Consumer />);
    const coldImage = screen.getByRole("img", { name: "offline printing" });
    await waitFor(() => expect(coldImage.getAttribute("data-loading")).toBe("false"));
    const coldSrc = coldImage.getAttribute("src");
    mounted.unmount();
    render(<Consumer />);
    const warmImage = screen.getByRole("img", { name: "offline printing" });
    await waitFor(() => expect(warmImage.getAttribute("data-loading")).toBe("false"));
    const result = {
      coldSrc, warmSrc: warmImage.getAttribute("src"),
      groups: [...new Set(resolve.mock.calls.map(([request]) => JSON.stringify(request.groups)))],
      presentationKeys: [...new Set(cacheWrites.mock.calls
        .filter(([, value]) => typeof value === "string").map(([key]) => key))],
      requestKeys: [...new Set(cacheWrites.mock.calls
        .filter(([, value]) => value !== null && typeof value === "object" && "sources" in value).map(([key]) => key))],
      mainMetadataLoads: fetchMock.mock.calls.filter(([url]) => url === "/scryfall-data.json").length,
    };
    cacheWrites.mockRestore();
    return result;
  }

  it.each([
    ["explicit", "nonmember", { set_code: "bad", collector_number: "9" }],
    ["explicit", "List", { set_code: "plst", collector_number: "6" }],
    ["name", "nonmember", { set_code: "bad", collector_number: "9" }],
    ["name", "List", { set_code: "plst", collector_number: "6" }],
  ] as const)("%s cold-offline rejection preserves ownerless identity (%s)", async (kind, _label, pair) => {
    const reference = await isolatedRun(kind);
    const candidate = await isolatedRun(kind, pair);
    const expected = `/installed/${kind === "explicit" ? viewer.id : source.id}/0-normal.jpg`;
    expect(reference.coldSrc).toBe(expected);
    expect(reference.warmSrc).toBe(expected);
    expect(candidate.coldSrc).toBe(reference.coldSrc);
    expect(candidate.warmSrc).toBe(reference.warmSrc);
    expect(candidate.groups).toEqual(reference.groups);
    expect(reference.presentationKeys).toEqual([expect.any(String)]);
    expect(reference.requestKeys).toEqual([expect.any(String)]);
    expect(candidate.presentationKeys).toEqual(reference.presentationKeys);
    expect(candidate.requestKeys).toEqual(reference.requestKeys);
    expect(reference.mainMetadataLoads).toBe(0);
    expect(candidate.mainMetadataLoads).toBe(0);
  });

  it.each(["explicit", "name"] as const)("%s warm-metadata offline rejection also preserves identity", async (kind) => {
    const reference = await isolatedRun(kind, undefined, true);
    const candidate = await isolatedRun(kind, { set_code: "bad", collector_number: "9" }, true);
    expect(reference.coldSrc).toBe(`/installed/${viewer.id}/${kind === "explicit" ? 1 : 0}-normal.jpg`);
    expect(candidate.coldSrc).toBe(reference.coldSrc);
    expect(candidate.warmSrc).toBe(reference.warmSrc);
    expect(candidate.groups).toEqual(reference.groups);
    expect(reference.presentationKeys).toEqual([expect.any(String)]);
    expect(reference.requestKeys).toEqual([expect.any(String)]);
    expect(candidate.presentationKeys).toEqual(reference.presentationKeys);
    expect(candidate.requestKeys).toEqual(reference.requestKeys);
    expect(reference.mainMetadataLoads).toBe(1);
    expect(candidate.mainMetadataLoads).toBe(1);
  });

  it("admits a known-Oracle owner offline without loading main metadata", async () => {
    const { useCardImage, fetchMock, prefs } = await setup({ cold: true, offline: true, installed: [owner, viewer] });
    const { result } = renderHook(() => useCardImage("Owner Card", {
      oracleId: A, faceName: "Back Card", faceIndex: 0, ownerPrinting: ownerPair,
    }));
    await waitFor(() => expect(result.current.src).toBe(`/installed/${owner.id}/0-normal.jpg`));
    expect(result.current.isLoading).toBe(false);
    expect(fetchMock).toHaveBeenCalledWith("/scryfall-printings.json");
    expect(fetchMock).not.toHaveBeenCalledWith("/scryfall-data.json");
    act(() => prefs.getState().setShowOwnerPrintings(false));
    await waitFor(() => expect(result.current.src).toBe(`/installed/${viewer.id}/0-normal.jpg`));
    expect(fetchMock).not.toHaveBeenCalledWith("/scryfall-data.json");
  });

  it("retries name-only owner admission when returning online", async () => {
    const { useCardImage, connectivity, fetchMock } = await setup({
      cold: true, offline: true, installed: [owner, viewer], canonical: true, nameSemantic: source,
    });
    const { result } = renderHook(() => useCardImage("Owner Card", { ownerPrinting: ownerPair }));
    await waitFor(() => expect(result.current.src).toBe(`/installed/${source.id}/0-normal.jpg`));
    expect(fetchMock).not.toHaveBeenCalledWith("/scryfall-data.json");
    act(() => connectivity.setState({ forcedOffline: false }));
    await waitFor(() => expect(result.current.src).toBe(`/installed/${owner.id}/0-normal.jpg`));
    expect(result.current.isLoading).toBe(false);
    expect(fetchMock).toHaveBeenCalledWith("/scryfall-data.json");
  });
});
