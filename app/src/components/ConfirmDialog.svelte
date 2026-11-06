<script lang="ts">
  import Modal from "./Modal.svelte";

  let {
    title,
    body,
    confirmLabel,
    cancelLabel,
    destructive = false,
    items = [],
    onconfirm,
    oncancel,
  }: {
    title: string;
    body: string;
    confirmLabel: string;
    cancelLabel: string;
    destructive?: boolean;
    /** Listed under the body (e.g. the devices affected). */
    items?: string[];
    onconfirm: () => void;
    oncancel: () => void;
  } = $props();
</script>

<Modal {title} onclose={oncancel} width={400}>
  <p>{body}</p>
  {#if items.length > 0}
    <ul>
      {#each items as item (item)}<li>{item}</li>{/each}
    </ul>
  {/if}
  {#snippet actions()}
    <!-- The safe choice has the focus. -->
    <button onclick={oncancel} data-autofocus>{cancelLabel}</button>
    <button class={destructive ? "primary danger-fill" : "primary"} onclick={onconfirm}>
      {confirmLabel}
    </button>
  {/snippet}
</Modal>

<style>
  p {
    margin: 0;
    color: var(--secondary);
  }
  ul {
    margin: 8px 0 0;
    padding-left: 18px;
    font-family: ui-monospace, monospace;
    font-size: 12px;
  }
  :global(button.danger-fill) {
    background: var(--danger);
    border-color: var(--danger);
  }
</style>
