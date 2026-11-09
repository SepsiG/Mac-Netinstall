<script lang="ts">
  // The event log: newest line first, collapsible, resizable (the splitter
  // above it), filterable by device, pausable, exportable as text (in the
  // current language, oldest first).
  //
  // Lines arrive in a plain buffer; the view catches up at most once a
  // frame and renders only the newest MAX_SHOWN, so a burst of events does
  // not redraw the list for every line. While paused the list stays as it
  // was; the buffer (and the export) keeps everything.
  import { onDestroy, untrack } from "svelte";
  import { toFailure } from "../lib/api";
  import { useBackend } from "../lib/context";
  import { clock } from "../lib/format";
  import { locale, t, type Lang } from "../lib/i18n";
  import {
    clampLogHeight,
    DEFAULT_LOG_HEIGHT,
    LOG_STEP,
    MAX_LOG_HEIGHT,
    MIN_LOG_HEIGHT,
    MIN_TABLE_HEIGHT,
    newestFirst,
  } from "../lib/logPanel";
  import { logText, type LogEntry } from "../lib/messages";
  import type { LogBuffer } from "../lib/state.svelte";
  import type { Failure } from "../lib/types";

  let {
    log,
    open = $bindable(),
    height = $bindable(null),
    onclear,
    onerror,
  }: {
    log: LogBuffer;
    open: boolean;
    /** The chosen height of the lines (CSS px); `null`: the default. */
    height?: number | null;
    onclear: () => void;
    onerror: (f: Failure) => void;
  } = $props();

  const backend = useBackend();
  const listId = $props.id();
  let filter = $state<string>("");
  let section = $state<HTMLElement>();
  let list = $state<HTMLOListElement>();

  // ---- What the view has caught up with (updated at most once a frame) ----

  /** Lines added to the buffer, as of the last frame. */
  let seen = $state(0);
  /** Lines in the buffer, as of the last frame. */
  let kept = $state(0);
  /** Bumped when the buffer was cleared (the shown lines change then). */
  let cleared = $state(0);
  let macs = $state<string[]>([]);
  const macSet = new Set<string>();
  /** Lines numbered below this were looked at for their MAC. */
  let scanned = 0;
  let clears = 0;
  let frame = 0;

  /** The line count when paused (`null`: following the log). */
  let pausedAt = $state<number | null>(null);
  /** Paused by scrolling away from the top (scrolling back resumes). */
  let pausedByScroll = false;

  function catchUp() {
    if (frame) cancelAnimationFrame(frame);
    frame = 0;
    if (log.clears !== clears) {
      clears = log.clears;
      macSet.clear();
      macs = [];
      filter = "";
      pausedAt = null;
      cleared++;
      scanned = log.added - log.lines.length;
    }
    const first = log.added - log.lines.length;
    let grew = false;
    for (let i = Math.max(0, scanned - first); i < log.lines.length; i++) {
      const mac = log.lines[i]!.mac;
      if (mac && !macSet.has(mac)) {
        macSet.add(mac);
        grew = true;
      }
    }
    scanned = log.added;
    if (grew) macs = [...macSet].sort();
    seen = log.added;
    kept = log.lines.length;
  }

  untrack(catchUp);
  $effect(() => {
    void log.version;
    if (!frame) frame = requestAnimationFrame(catchUp);
  });

  const shown = $derived.by(() => {
    void cleared;
    return newestFirst(log.lines, log.added - log.lines.length, pausedAt ?? seen, filter);
  });
  const fresh = $derived(pausedAt === null ? 0 : seen - pausedAt);

  // Following: the newest line (at the top) stays in view.
  $effect(() => {
    void shown;
    if (pausedAt === null && list) list.scrollTop = 0;
  });

  function pause(byScroll: boolean) {
    pausedAt = seen;
    pausedByScroll = byScroll;
  }

  function resume() {
    catchUp();
    pausedAt = null;
  }

  /** Reading further down pauses; back at the very top follows again
   *  (unless paused by the button). */
  function scrolled() {
    if (!list) return;
    const atTop = list.scrollTop <= 2;
    if (pausedAt === null && !atTop) pause(true);
    else if (pausedAt !== null && pausedByScroll && atTop) resume();
  }

  function clear() {
    onclear();
    catchUp();
  }

  function line(lang: Lang, e: LogEntry): string {
    const mark = e.level === "error" ? "✖ " : e.level === "warning" ? "⚠ " : "";
    return [clock(e.at, lang), e.mac ?? "", mark + logText(lang, e)].filter((s) => s !== "").join("  ");
  }

  /** Every kept line (of the chosen device), oldest first, as log files go. */
  async function exportLog() {
    const lang = $locale;
    const text = log.lines.flatMap((e) => (!filter || e.mac === filter ? [line(lang, e)] : [])).join("\n") + "\n";
    try {
      await backend.exportLog(text);
    } catch (e) {
      onerror(toFailure(e));
    }
  }

  // ---- The splitter: the height of the lines ----

  let lines = $state(clampLogHeight(untrack(() => height) ?? DEFAULT_LOG_HEIGHT));
  /** The largest height the layout allows now. */
  let limit = $state(MAX_LOG_HEIGHT);

  /** The flexible element above (the device table) gives up room, down to
   *  MIN_TABLE_HEIGHT; without one there is no layout limit. */
  function measure(): number {
    let el = section?.previousElementSibling;
    while (el && getComputedStyle(el).flexGrow !== "1") el = el.previousElementSibling;
    limit = el ? lines + el.getBoundingClientRect().height - MIN_TABLE_HEIGHT : MAX_LOG_HEIGHT;
    return limit;
  }

  function fit(want: number) {
    lines = clampLogHeight(want, measure());
  }

  // The chosen height (also once the settings are loaded, and when opened
  // again), within the room.
  $effect(() => {
    void open;
    const want = height ?? DEFAULT_LOG_HEIGHT;
    untrack(() => fit(want));
  });

  $effect(() => {
    const onResize = () => fit(height ?? DEFAULT_LOG_HEIGHT);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  });

  let saveTimer: ReturnType<typeof setTimeout> | undefined;
  /** Remember `h` as chosen (a failure to store only loses the choice). */
  function choose(h: number, later = false) {
    height = h;
    clearTimeout(saveTimer);
    saveTimer = undefined;
    const save = () => {
      saveTimer = undefined;
      backend.setLogPanelHeight(h).catch(() => {});
    };
    if (later) saveTimer = setTimeout(save, 300);
    else save();
  }

  onDestroy(() => {
    if (frame) cancelAnimationFrame(frame);
    if (saveTimer !== undefined) {
      clearTimeout(saveTimer);
      backend.setLogPanelHeight(lines).catch(() => {});
    }
  });

  function keydown(e: KeyboardEvent) {
    const step = e.key === "ArrowUp" ? LOG_STEP : e.key === "ArrowDown" ? -LOG_STEP : 0;
    if (!step) return;
    e.preventDefault();
    fit(lines + step);
    choose(lines, true);
  }

  function pointerdown(e: PointerEvent) {
    if (e.button !== 0) return;
    e.preventDefault();
    const startY = e.clientY;
    const start = lines;
    const move = (m: PointerEvent) => {
      // Up makes the log taller.
      fit(start + (startY - m.clientY));
    };
    const end = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", end);
      window.removeEventListener("pointercancel", end);
      choose(lines);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", end);
    window.addEventListener("pointercancel", end);
  }

  function reset() {
    fit(DEFAULT_LOG_HEIGHT);
    choose(DEFAULT_LOG_HEIGHT);
  }
</script>

<section class="log" class:open bind:this={section}>
  {#if open}
    <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
    <div
      class="splitter"
      role="separator"
      tabindex="0"
      aria-orientation="horizontal"
      aria-controls={listId}
      aria-label={$t("log.resize")}
      title={$t("log.resize")}
      aria-valuenow={lines}
      aria-valuemin={MIN_LOG_HEIGHT}
      aria-valuemax={Math.max(MIN_LOG_HEIGHT, Math.round(limit))}
      onkeydown={keydown}
      onpointerdown={pointerdown}
      ondblclick={reset}
    ></div>
  {/if}
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
      {#if fresh > 0}
        <span class="badge">{$t("log.new_lines", { count: fresh })}</span>
      {/if}
      {#if pausedAt === null}
        <button class="small" onclick={() => pause(false)} title={$t("log.pause_help")}>{$t("log.pause")}</button>
      {:else}
        <button class="small" onclick={resume} title={$t("log.resume_help")}>{$t("log.resume")}</button>
      {/if}
      <button class="small" onclick={clear} disabled={kept === 0}>{$t("log.clear")}</button>
      <button class="small" onclick={exportLog} disabled={kept === 0} title={$t("log.export_help")}>{$t("log.export")}</button>
    {/if}
  </header>
  {#if open}
    <ol class="lines mono" id={listId} bind:this={list} style:height="{lines}px" onscroll={scrolled}>
      {#each shown as e (e)}
        <li class={e.level}>{line($locale, e)}</li>
      {:else}
        <li class="secondary">{$t("log.empty")}</li>
      {/each}
    </ol>
  {/if}
</section>

<style>
  .log {
    position: relative;
    border-top: 1px solid var(--separator);
    background: var(--panel-alt);
  }
  .splitter {
    position: absolute;
    top: -4px;
    left: 0;
    right: 0;
    height: 7px;
    z-index: 1;
    cursor: row-resize;
    touch-action: none;
    outline: none;
  }
  .splitter:hover,
  .splitter:focus-visible {
    background: linear-gradient(transparent 2px, var(--accent) 2px, var(--accent) 5px, transparent 5px);
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
  .badge {
    padding: 0 7px;
    border-radius: 9px;
    font-size: 11px;
    color: var(--accent-text);
    background: var(--accent);
    white-space: nowrap;
  }
  .lines {
    margin: 0;
    padding: 4px 12px 6px;
    overflow: auto;
    overflow-anchor: none;
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
