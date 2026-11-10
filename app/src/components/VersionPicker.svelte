<script lang="ts">
  // Choose the RouterOS version of a profile: a channel ("always the
  // newest", with what it names now), a version from MikroTik's list
  // (7.x, or the 6.x archive), or one typed by hand and checked against
  // download.mikrotik.com. The list's age and source are shown, with a
  // refresh button.
  import { onMount } from "svelte";
  import { toFailure } from "../lib/api";
  import { useBackend } from "../lib/context";
  import { locale, t } from "../lib/i18n";
  import { describe } from "../lib/messages";
  import { validVersion } from "../lib/profile";
  import type { Failure, VersionsView } from "../lib/types";
  import { isV6, listAgeHours, versionGroups } from "../lib/versions";

  let {
    value = $bindable(),
    custom = $bindable(),
    exact = $bindable(),
    error,
  }: {
    /** The picked channel or version. */
    value: string;
    /** Typing a version by hand. */
    custom: boolean;
    exact: string;
    /** A message key for the typed version, if invalid. */
    error?: string;
  } = $props();

  const CUSTOM = "__custom";
  const backend = useBackend();
  let view = $state<VersionsView | null>(null);
  let loadError = $state<Failure | null>(null);
  let refreshing = $state(false);
  let query = $state("");
  /** `exists`, `missing`, `unknown` (could not check) for the typed text. */
  let check = $state<{ version: string; result: "exists" | "missing" | "unknown" } | null>(null);
  let now = $state(Date.now());

  const EMPTY: VersionsView = { channels: [], versions: [], fetched_unix: 0, source: "seed", error: null };
  const groups = $derived(versionGroups(view ?? EMPTY, query, (c) => $t(`channel.${c}`)));
  const spec = $derived(custom ? exact.trim() : value);
  const known = $derived(
    (view?.channels.some((c) => c.channel === value) ?? false) ||
      (view?.versions.some((l) => l.version === value) ?? false) ||
      ["stable", "long-term", "testing", "development", "v6-long-term"].includes(value),
  );
  const age = $derived(view ? listAgeHours(view, now) : null);

  onMount(() => void load(false));

  async function load(refresh: boolean) {
    refreshing = true;
    try {
      view = await backend.availableVersions(refresh);
      loadError = null;
      now = Date.now();
    } catch (e) {
      loadError = toFailure(e);
    }
    refreshing = false;
  }

  function choose(v: string) {
    if (v === CUSTOM) {
      custom = true;
      return;
    }
    custom = false;
    value = v;
  }

  /** Wait this long after typing before asking whether it exists. */
  const TYPING_PAUSE_MS = 400;
  let checkSeq = 0;
  $effect(() => {
    const v = exact.trim();
    if (!custom || !validVersion(v)) {
      check = null;
      return;
    }
    const mine = ++checkSeq;
    const timer = setTimeout(() => {
      backend.checkVersionExists(v).then(
        (ok) => {
          if (mine === checkSeq) check = { version: v, result: ok ? "exists" : "missing" };
        },
        () => {
          if (mine === checkSeq) check = { version: v, result: "unknown" };
        },
      );
    }, TYPING_PAUSE_MS);
    return () => clearTimeout(timer);
  });

  function tags(channels: string[]): string {
    return channels.map((c) => $t(`channel.${c}`)).join(", ");
  }
</script>

<div class="picker">
  <div class="row">
    <input
      type="search"
      bind:value={query}
      placeholder={$t("versions.search")}
      aria-label={$t("versions.search")}
      size="14"
    />
    <select
      aria-label={$t("profile.version")}
      value={custom ? CUSTOM : value}
      onchange={(e) => choose(e.currentTarget.value)}
    >
      {#if !custom && !known}
        <option value={value}>{value}</option>
      {/if}
      {#if groups.channels.length > 0}
        <optgroup label={$t("versions.group_channels")}>
          {#each groups.channels as c (c.channel)}
            <option value={c.channel}>
              {$t(`channel.${c.channel}`)} ({c.version ?? $t("versions.unknown")})
            </option>
          {/each}
        </optgroup>
      {/if}
      {#if groups.v7.length > 0}
        <optgroup label={$t("versions.group_v7")}>
          {#each groups.v7 as l (l.version)}
            <option value={l.version}>
              {l.version} · {tags(l.channels)}{l.date ? ` · ${l.date}` : ""}
            </option>
          {/each}
        </optgroup>
      {/if}
      {#if groups.v6.length > 0}
        <optgroup label={$t("versions.group_v6")}>
          {#each groups.v6 as l (l.version)}
            <option value={l.version}>
              {l.version} · {tags(l.channels)}{l.date ? ` · ${l.date}` : ""}
            </option>
          {/each}
        </optgroup>
      {/if}
      <option value={CUSTOM}>{$t("versions.custom")}</option>
    </select>
  </div>

  {#if custom}
    <div class="row">
      <input
        type="text"
        bind:value={exact}
        placeholder={$t("profile.exact_placeholder")}
        aria-label={$t("versions.custom_label")}
        size="12"
      />
      {#if check && check.version === exact.trim()}
        <span class="check-{check.result}" role="status">{$t(`versions.check_${check.result}`)}</span>
      {/if}
    </div>
    {#if error}<div class="field-error" role="alert">{$t(error)}</div>{/if}
  {/if}

  <div class="row secondary small-text">
    <span>
      {#if !view}
        {$t("versions.loading")}
      {:else if age === null}
        {$t("versions.source_seed")}
      {:else}
        {$t("versions.source_age", { hours: age })}
      {/if}
      {#if view?.error}· {$t("versions.refresh_failed")}{/if}
    </span>
    <button type="button" class="small" disabled={refreshing} onclick={() => load(true)}>{$t("versions.refresh")}</button>
  </div>
  {#if loadError}<div class="field-error">{describe($locale, loadError)}</div>{/if}

  {#if isV6(spec)}
    <div class="warning" role="note">{$t("versions.v6_warning")}</div>
  {/if}
</div>

<style>
  .picker {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  select {
    max-width: 100%;
  }
  .small-text {
    font-size: 11px;
  }
  .warning {
    font-size: 12px;
    color: var(--warning, #b26b00);
    white-space: normal;
  }
  .field-error {
    color: var(--danger);
    font-size: 12px;
  }
  .check-missing {
    color: var(--danger);
    font-size: 12px;
  }
  .check-exists,
  .check-unknown {
    color: var(--secondary);
    font-size: 12px;
  }
</style>
