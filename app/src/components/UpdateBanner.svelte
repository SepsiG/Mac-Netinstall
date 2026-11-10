<script lang="ts">
  // The update notice at the top of the main window: a newer release is
  // on GitHub. Non-modal; the release notes open in a sheet. The download
  // (verified against the published SHA-256) runs in Rust, which also
  // places the verified DMG in Downloads; the window never handles a URL
  // or a path.
  //
  // `hidden` hides it while devices are being prepared or flashed; it
  // stays mounted, so a running download keeps its progress. The DMG is
  // opened (once, automatically) only while it is not hidden, and the
  // release notes sheet closes while hidden.
  import { onMount } from "svelte";
  import { toFailure } from "../lib/api";
  import { useBackend } from "../lib/context";
  import { bytes, percent } from "../lib/format";
  import { locale, t } from "../lib/i18n";
  import { describe, hint } from "../lib/messages";
  import { noteBlocks } from "../lib/releaseNotes";
  import type { DownloadedView, Failure, UpdateProgress, UpdateView } from "../lib/types";
  import Modal from "./Modal.svelte";

  let {
    update,
    hidden = false,
    onlater,
    onupdate,
  }: {
    update: UpdateView;
    hidden?: boolean;
    /** "Later": hide it until the next start. */
    onlater: () => void;
    /** A new state (after skipping the version). */
    onupdate: (v: UpdateView) => void;
  } = $props();

  const backend = useBackend();
  let notesOpen = $state(false);
  let downloading = $state(false);
  let progress = $state<UpdateProgress | null>(null);
  let done = $state<DownloadedView | null>(null);
  let error = $state<Failure | null>(null);
  /** The DMG was opened automatically (or that failed). */
  let autoOpened = $state(false);
  let openFailed = $state(false);

  const version = $derived(update.latest?.version ?? "");
  const blocks = $derived(noteBlocks(update.latest?.notes ?? ""));
  const progressText = $derived(
    progress === null
      ? ""
      : progress.total
        ? `${percent(progress.done, progress.total)}%`
        : bytes(progress.done, $locale),
  );

  onMount(() => {
    let unlisten: (() => void) | null = null;
    let gone = false;
    void backend
      .onUpdateProgress((p) => {
        if (downloading) progress = p;
      })
      .then((u) => {
        if (gone) u();
        else unlisten = u;
      });
    return () => {
      gone = true;
      unlisten?.();
    };
  });

  async function download() {
    error = null;
    done = null;
    progress = null;
    autoOpened = false;
    openFailed = false;
    downloading = true;
    try {
      done = await backend.downloadUpdate();
    } catch (e) {
      error = toFailure(e);
    } finally {
      downloading = false;
    }
  }

  async function openDmg() {
    try {
      await backend.openDownloadedUpdate();
      openFailed = false;
    } catch (e) {
      openFailed = true;
      error = toFailure(e);
    }
  }

  // Open the verified DMG once nothing is being flashed.
  $effect(() => {
    if (done && !hidden && !autoOpened) {
      autoOpened = true;
      void openDmg();
    }
  });

  async function skip() {
    try {
      onupdate(await backend.skipUpdate(version));
    } catch (e) {
      error = toFailure(e);
    }
  }

  async function openPage() {
    try {
      await backend.openReleasePage();
    } catch (e) {
      error = toFailure(e);
    }
  }
</script>

<div class="update" role="status" {hidden}>
  <div class="text">
    {#if done}
      <div>{$t("update.downloaded", { file: done.file_name })}</div>
      <div class="secondary">{$t("update.install_hint")}</div>
      {#if openFailed}<div class="secondary">{$t("update.not_opened")}</div>{/if}
    {:else if downloading}
      <div>{$t("update.downloading", { version, progress: progressText })}</div>
      {#if progress?.total}
        <progress max={progress.total} value={progress.done}></progress>
      {/if}
    {:else}
      <div>{$t("update.available", { version })}</div>
    {/if}
    {#if error}
      <div class="error" role="alert">
        {describe($locale, error)}
        {#if hint($locale, error.code)}<span class="secondary">{hint($locale, error.code)}</span>{/if}
      </div>
    {/if}
  </div>
  <div class="actions">
    {#if !done && !downloading}
      <button class="small" onclick={() => (notesOpen = true)}>{$t("update.whats_new")}</button>
      <button class="small primary" onclick={download}>{$t("update.download")}</button>
      <button class="small" onclick={skip}>{$t("update.skip")}</button>
    {/if}
    {#if done}
      <button class="small primary" onclick={openDmg}>{$t("update.open")}</button>
    {/if}
    {#if !downloading}
      <button class="small" onclick={onlater}>{done ? $t("update.close") : $t("update.later")}</button>
    {/if}
  </div>
</div>

{#if notesOpen && !hidden}
  <Modal title={$t("update.notes_title", { version })} onclose={() => (notesOpen = false)} id="release_notes" width={560} minWidth={360} minHeight={200}>
    <div class="notes">
      {#each blocks as b, i (i)}
        {#if b.kind === "heading"}
          <h4>{b.text}</h4>
        {:else if b.kind === "item"}
          <div class="item">• {b.text}</div>
        {:else}
          <p>{b.text}</p>
        {/if}
      {:else}
        <p class="secondary">{$t("update.notes_empty")}</p>
      {/each}
    </div>
    {#snippet actions()}
      <button onclick={openPage}>{$t("update.open_page")}</button>
      <button class="primary" onclick={() => (notesOpen = false)} data-autofocus>{$t("update.close")}</button>
    {/snippet}
  </Modal>
{/if}

<style>
  .update {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 10px;
    padding: 6px 12px;
    background: color-mix(in srgb, var(--accent) 10%, var(--panel));
    border-bottom: 1px solid var(--separator);
  }
  .update[hidden] {
    display: none;
  }
  .text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .actions {
    display: flex;
    gap: 6px;
    flex-shrink: 0;
  }
  .secondary {
    color: var(--secondary);
    font-size: 12px;
  }
  .error {
    color: var(--danger);
  }
  progress {
    width: 240px;
  }
  .notes {
    user-select: text;
  }
  .notes h4 {
    margin: 10px 0 4px;
    font-size: 13px;
  }
  .notes p {
    margin: 4px 0;
    white-space: pre-wrap;
  }
  .notes .item {
    margin: 2px 0 2px 8px;
  }
</style>
