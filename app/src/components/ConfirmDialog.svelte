<script lang="ts">
  import Modal from "./Modal.svelte";

  let {
    title,
    body,
    confirmLabel,
    cancelLabel,
    destructive = false,
    onconfirm,
    oncancel,
  }: {
    title: string;
    body: string;
    confirmLabel: string;
    cancelLabel: string;
    destructive?: boolean;
    onconfirm: () => void;
    oncancel: () => void;
  } = $props();
</script>

<Modal {title} onclose={oncancel} width={400}>
  <p>{body}</p>
  {#snippet actions()}
    <button onclick={oncancel}>{cancelLabel}</button>
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
  :global(button.danger-fill) {
    background: var(--danger);
    border-color: var(--danger);
  }
</style>
