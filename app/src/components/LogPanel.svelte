<script lang="ts">
  // The event log: collapsible, filterable by device, exportable as text
  // (in the current language).
  import { toFailure } from "../lib/api";
  import { useBackend } from "../lib/context";
  import { clock } from "../lib/format";
  import { locale, t, type Lang } from "../lib/i18n";
  import { logText, type LogEntry } from "../lib/messages";
  import type { Failure } from "../lib/types";

  let {
    entries,
    open = $bindable(),
    onclear,
    onerror,
  }: {
    entries: LogEntry[];
    open: boolean;
    onclear: () => void;
    onerror: (f: Failure) => void;
  } = $props();

  const backend = useBackend();
  let filter = $state<string>("");

  const macs = $derived([...new Set(entries.flatMap((e) => (e.mac ? [e.mac] : [])))].sort());
  const shown = $derived(filter ? entries.filter((e) => e.mac === filter) : entries);

  function line(lang: Lang, e: LogEntry): string {
    const mark = e.level === "error" ? "✖ " : e.level === "warning" ? "⚠ " : "";
    return [clock(e.at, lang), e.mac ?? "", mark + logText(lang, e)].filter((s) => s !== "").join("  ");
  }

  async function exportLog() {
    const text = shown.map((e) => line($locale, e)).join("\n") + "\n";
    try {
      await backend.exportLog(text);
    } catch (e) {
      onerror(toFailure(e));
    }
  }
</script>

<section class="log" class:open>
  <header>
    <button class="disclosure" aria-expanded={open} onclick={() => (open = !open)}>
      <span class="chevron">{open ? "▾" : "▸"}</span>
      {$t("log.title")}
    </button>
    {#if open}
      <select bind:value={filter} aria-label={$t("log.all_devices")}>
        <option value="">{$t("log.all_devices")}</option>
        {#each macs as mac (mac)}
          <option value={mac}>{mac}</option>
        {/each}
      </select>
      <span class="spacer"></span>
      <button class="small" onclick={onclear} disabled={entries.length === 0}>{$t("log.clear")}</button>
      <button class="small" onclick={exportLog} disabled={shown.length === 0}>{$t("log.export")}</button>
    {/if}
  </header>
  {#if open}
    <ol class="lines mono">
      {#each shown as e, i (i)}
        <li class={e.level}>{line($locale, e)}</li>
      {:else}
        <li class="secondary">{$t("log.empty")}</li>
      {/each}
    </ol>
  {/if}
</section>

<style>
  .log {
    border-top: 1px solid var(--separator);
    background: var(--panel-alt);
  }
  header {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 3px 10px;
  }
  .disclosure {
    border: none;
    background: none;
    box-shadow: none;
    padding: 0 4px;
    font-weight: 600;
  }
  .chevron {
    display: inline-block;
    width: 12px;
  }
  .spacer {
    flex: 1;
  }
  .lines {
    height: 150px;
    margin: 0;
    padding: 4px 12px 6px;
    overflow: auto;
    list-style: none;
    background: var(--panel);
    border-top: 1px solid var(--separator);
    -webkit-user-select: text;
    user-select: text;
  }
  .lines li {
    white-space: pre-wrap;
  }
  .error {
    color: var(--danger);
  }
  .warning {
    color: var(--warning);
  }
</style>
