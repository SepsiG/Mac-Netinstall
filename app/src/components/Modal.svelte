<script lang="ts">
  // A sheet-like modal dialog. Escape calls `onclose`.
  import type { Snippet } from "svelte";

  let {
    title,
    onclose,
    children,
    actions,
    width = 440,
  }: {
    title: string;
    onclose: () => void;
    children: Snippet;
    actions?: Snippet;
    width?: number;
  } = $props();

  const id = `modal-${Math.random().toString(36).slice(2)}`;

  function keydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.stopPropagation();
      onclose();
    }
  }
</script>

<div class="backdrop" role="presentation" onkeydown={keydown}>
  <div class="sheet" role="dialog" aria-modal="true" aria-labelledby={id} style:width="{width}px">
    <h2 {id}>{title}</h2>
    <div class="body">{@render children()}</div>
    {#if actions}
      <div class="actions">{@render actions()}</div>
    {/if}
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding-top: 48px;
    background: rgb(0 0 0 / 0.18);
    z-index: 10;
  }
  .sheet {
    max-width: calc(100vw - 32px);
    max-height: calc(100vh - 80px);
    overflow: auto;
    padding: 16px 18px 14px;
    border-radius: 10px;
    background: var(--panel);
    box-shadow: var(--shadow);
    border: 1px solid var(--separator);
  }
  h2 {
    margin: 0 0 10px;
    font-size: 13px;
    font-weight: 600;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 14px;
  }
</style>
