import type { PromptEnhanceRequest, PromptEnhancerProvider } from "../types";

const SETTINGS_KEY = "tersh.promptEnhancer.settings";
const API_KEY_SESSION_KEY = "tersh.promptEnhancer.apiKey";
// Selected project is scoped PER VPS connection (per host) / per local project,
// so 50 open VPS terminals each keep their own selection — not one shared blob.
const SELECTED_BRAIN_MAP_KEY = "tersh.promptEnhancer.selectedBrainByScope";

/// Stable key for a session's selected project index. Remote is keyed PER
/// SESSION (not per host): each SSH tab is its own session id, so any number of
/// concurrent sessions to the SAME VPS — even indexing different folders — can
/// never share or clobber each other's selection. Two tabs = two session ids =
/// two scopes, full stop. Local = the project root. Null when there's nothing
/// indexable (e.g. on the host list).
export function brainScopeKey(
  kind: "local" | "remote",
  remoteSessionId: string | null | undefined,
  localRoot: string | null | undefined,
): string | null {
  if (kind === "remote") return remoteSessionId ? `session:${remoteSessionId}` : null;
  // A local terminal with no resolved cwd is not indexable — return null so it
  // agrees with the Drawer (which also yields null) instead of collapsing every
  // such terminal into one shared `local:` bucket.
  return localRoot && localRoot.trim() ? `local:${localRoot}` : null;
}

export interface PromptEnhancerSettings {
  provider: PromptEnhancerProvider;
  baseUrl: string;
  model: string;
  /** Optional embedding model. Empty = TF-IDF + n-gram retrieval only.
   *  Set explicitly to enable semantic search at the same provider's
   *  /embeddings endpoint. Never silently defaulted. */
  embeddingModel: string;
}

export interface PromptEnhancerConfig extends PromptEnhancerSettings {
  apiKey: string;
}

const DEFAULT_SETTINGS: PromptEnhancerSettings = {
  provider: "openrouter",
  baseUrl: "",
  model: "deepseek/deepseek-v4-flash",
  embeddingModel: "",
};

const DIRECT_DEFAULTS: Record<PromptEnhancerProvider, Pick<PromptEnhancerSettings, "baseUrl" | "model">> = {
  openrouter: { baseUrl: "", model: "deepseek/deepseek-v4-flash" },
  deepseek: { baseUrl: "", model: "deepseek-v4-flash" },
  mimo: { baseUrl: "", model: "" },
  custom: { baseUrl: "", model: "" },
};

/// Curated dropdown options for the OpenRouter chat-model field. Picked for
/// solid tool-call support, which the prompt enhancer agent loop relies on.
/// Models change over time — if a user's preferred model isn't here they can
/// switch to "Custom" and type any OpenRouter route.
export const OPENROUTER_MODEL_PRESETS: ReadonlyArray<{ value: string; label: string }> = [
  { value: "deepseek/deepseek-v4-flash", label: "DeepSeek V4 Flash" },
  { value: "deepseek/deepseek-v4-pro", label: "DeepSeek V4 Pro (reasoning)" },
  { value: "anthropic/claude-sonnet-4-5", label: "Claude Sonnet 4.5" },
  { value: "anthropic/claude-opus-4-1", label: "Claude Opus 4.1" },
  { value: "openai/gpt-4o", label: "GPT-4o" },
  { value: "openai/gpt-4o-mini", label: "GPT-4o mini" },
  { value: "qwen/qwen3-coder", label: "Qwen3 Coder" },
];

export function defaultPromptEnhancerSettings(provider: PromptEnhancerProvider): PromptEnhancerSettings {
  const defaults = DIRECT_DEFAULTS[provider] ?? DIRECT_DEFAULTS.openrouter;
  return { provider, ...defaults, embeddingModel: "" };
}

export function loadPromptEnhancerSettings(): PromptEnhancerSettings {
  try {
    const raw = window.localStorage.getItem(SETTINGS_KEY);
    if (!raw) return DEFAULT_SETTINGS;
    const parsed = JSON.parse(raw) as Partial<PromptEnhancerSettings>;
    const provider = isProvider(parsed.provider) ? parsed.provider : DEFAULT_SETTINGS.provider;
    const defaults = defaultPromptEnhancerSettings(provider);
    const rawModel = typeof parsed.model === "string" ? parsed.model.trim() : "";
    const rawEmbedding =
      typeof parsed.embeddingModel === "string" ? parsed.embeddingModel.trim() : "";
    return {
      provider,
      baseUrl: typeof parsed.baseUrl === "string" ? parsed.baseUrl : defaults.baseUrl,
      model: normalizePromptEnhancerModel(provider, rawModel || defaults.model),
      embeddingModel: rawEmbedding,
    };
  } catch {
    return DEFAULT_SETTINGS;
  }
}

export function savePromptEnhancerSettings(settings: PromptEnhancerSettings) {
  window.localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
}

export function loadPromptEnhancerApiKey(): string {
  return window.sessionStorage.getItem(API_KEY_SESSION_KEY) ?? "";
}

export function savePromptEnhancerApiKey(apiKey: string) {
  if (apiKey.trim()) {
    window.sessionStorage.setItem(API_KEY_SESSION_KEY, apiKey);
  } else {
    window.sessionStorage.removeItem(API_KEY_SESSION_KEY);
  }
}

// sessionStorage, not localStorage: the selection is keyed by session id (which
// is regenerated every connect), so persisting it across app restarts would
// only accumulate dead entries. Within a run it survives tab switches; on a
// fresh launch the project is re-detected from the VPS instead.
function loadSelectedBrainMap(): Record<string, string> {
  try {
    const raw = window.sessionStorage.getItem(SELECTED_BRAIN_MAP_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw);
    return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : {};
  } catch {
    return {};
  }
}

export function loadPromptEnhancerBrainId(scopeKey: string | null): string | null {
  if (!scopeKey) return null;
  const value = loadSelectedBrainMap()[scopeKey];
  return value && value.trim() ? value : null;
}

export function savePromptEnhancerBrainId(scopeKey: string | null, brainId: string | null) {
  if (!scopeKey) return;
  const map = loadSelectedBrainMap();
  if (brainId && brainId.trim()) {
    map[scopeKey] = brainId;
  } else {
    delete map[scopeKey];
  }
  window.sessionStorage.setItem(SELECTED_BRAIN_MAP_KEY, JSON.stringify(map));
}

// Remember which folders on each VPS have been indexed, so on reconnect we can
// hydrate them from <root>/.tersh/ even if discovery doesn't surface them (e.g.
// a folder with no package.json). Keyed by host id → list of absolute roots.
// Stale entries (folder/index deleted) just fail the hydrate check and are
// harmless. localStorage so it survives app restarts.
const INDEXED_ROOTS_KEY = "tersh.promptEnhancer.indexedRootsByHost";

function loadIndexedRootsMap(): Record<string, string[]> {
  try {
    const raw = window.localStorage.getItem(INDEXED_ROOTS_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw);
    return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : {};
  } catch {
    return {};
  }
}

export function indexedRootsForHost(hostId: string | null | undefined): string[] {
  if (!hostId) return [];
  const list = loadIndexedRootsMap()[hostId];
  return Array.isArray(list) ? list.filter(r => typeof r === "string") : [];
}

export function rememberIndexedRoot(hostId: string, root: string) {
  const r = root.replace(/\/+$/, "");
  if (!hostId || !r) return;
  const map = loadIndexedRootsMap();
  const list = new Set(map[hostId] ?? []);
  list.add(r);
  map[hostId] = [...list];
  window.localStorage.setItem(INDEXED_ROOTS_KEY, JSON.stringify(map));
}

export function forgetIndexedRoot(hostId: string, root: string) {
  const r = root.replace(/\/+$/, "");
  if (!hostId) return;
  const map = loadIndexedRootsMap();
  const list = (map[hostId] ?? []).filter(x => x !== r);
  if (list.length) map[hostId] = list; else delete map[hostId];
  window.localStorage.setItem(INDEXED_ROOTS_KEY, JSON.stringify(map));
}

export function loadPromptEnhancerConfig(): PromptEnhancerConfig {
  return {
    ...loadPromptEnhancerSettings(),
    apiKey: loadPromptEnhancerApiKey(),
  };
}

export function buildPromptEnhanceRequest(
  prompt: string,
  sessionId?: string | null,
  scopeKey?: string | null,
): PromptEnhanceRequest {
  const config = loadPromptEnhancerConfig();
  if (!config.apiKey.trim()) {
    throw new Error("Add a provider API key in the drawer first.");
  }
  if (!config.model.trim()) {
    throw new Error("Choose a model in the drawer first.");
  }
  const model = normalizePromptEnhancerModel(config.provider, config.model);
  validatePromptEnhancerModel(config.provider, model);
  return {
    provider: config.provider,
    base_url: config.baseUrl.trim() || null,
    api_key: config.apiKey,
    model,
    prompt,
    // Per-VPS: the project selected for THIS connection, not a global one.
    brain_id: loadPromptEnhancerBrainId(scopeKey ?? null),
    session_id: sessionId ?? null,
    embedding_model: config.embeddingModel.trim() || null,
  };
}

/// Build a BrainIndexAiConfig for brain_enable_local / brain_enable_remote /
/// brain_refresh. Returns null if the user hasn't entered an API key — the
/// caller should pass null in that case (the backend can still index without
/// AI; only the LLM project_digest synthesis is skipped).
export function buildBrainIndexAiConfig(): import("../types").BrainIndexAiConfig | null {
  const config = loadPromptEnhancerConfig();
  if (!config.apiKey.trim() || !config.model.trim()) {
    return null;
  }
  return {
    provider: config.provider,
    base_url: config.baseUrl.trim() || null,
    api_key: config.apiKey,
    model: normalizePromptEnhancerModel(config.provider, config.model),
    embedding_model: config.embeddingModel.trim() || null,
  };
}

export function providerLabel(provider: PromptEnhancerProvider): string {
  switch (provider) {
    case "openrouter": return "OpenRouter";
    case "deepseek": return "DeepSeek";
    case "mimo": return "MiMo";
    case "custom": return "Custom";
  }
}

function isProvider(value: unknown): value is PromptEnhancerProvider {
  return value === "openrouter" || value === "deepseek" || value === "mimo" || value === "custom";
}

export function normalizePromptEnhancerModel(
  provider: PromptEnhancerProvider,
  model: string,
): string {
  const trimmed = model.trim();
  if (provider !== "deepseek") return trimmed;
  if (trimmed === "deepseek-chat") return "deepseek-v4-flash";
  if (trimmed === "deepseek-reasoner") return "deepseek-v4-pro";
  return trimmed;
}

export function validatePromptEnhancerModel(
  provider: PromptEnhancerProvider,
  model: string,
) {
  if (provider !== "deepseek") return;
  if (model === "deepseek-v4-flash" || model === "deepseek-v4-pro") return;
  throw new Error("DeepSeek direct supports deepseek-v4-flash or deepseek-v4-pro.");
}
