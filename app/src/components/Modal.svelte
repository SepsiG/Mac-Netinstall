<script lang="ts" module>
  // Open modals, innermost last: only the topmost one handles keys.
  const stack: symbol[] = [];
</script>

<script lang="ts">
  // A sheet-like modal dialog. While open: focus starts on the element
  // marked `data-autofocus` (the safe choice; else the first control), Tab
  // stays inside, Escape (anywhere in the window) calls `onclose`, and the
  // rest of the window is `inert`. Focus goes back where it was on close.
  import { onMount, type Snippet } from "svelte";

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
  const me = Symbol(id);
  let backdrop: HTMLDivElement;
  let sheet: HTMLDivElement;

  const FOCUSABLE =
    'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [href], [tabindex]:not([tabindex="-1"])';

  function focusables(): HTMLElement[] {
    return [...sheet.querySelectorAll<HTMLElement>(FOCUSABLE)];
  }

  /** Make everything outside the dialog inert: the siblings of each
   *  element on the way from the backdrop up to `body`. Returns the undo. */
  function inertOutside(): () => void {
    const changed: Element[] = [];
    let node: Element = backdrop;
    while (node.parentElement && node !== document.body) {
      for (const sibling of node.parentElement.children) {
        if (sibling !== node && !sibling.hasAttribute("inert")) {
          sibling.setAttribute("inert", "");
          changed.push(sibling);
        }
      }
      node = node.parentElement;
    }
    return () => changed.forEach((e) => e.removeAttribute("inert"));
  }

  function keydown(e: KeyboardEvent) {
    if (stack[stack.length - 1] !== me) return;
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onclose();
    } else if (e.key === "Tab") {
      const list = focusables();
      if (list.length === 0) return;
      const at = list.indexOf(document.activeElement as HTMLElement);
      const next = e.shiftKey ? (at <= 0 ? list.length - 1 : at - 1) : at === list.length - 1 ? 0 : at + 1;
      e.preventDefault();
      list[next]!.focus();
    }
  }

  onMount(() => {
    const before = document.activeElement as HTMLElement | null;
    stack.push(me);
    const undoInert = inertOutside();
    const first = sheet.querySelector<HTMLElement>("[data-autofocus]") ?? focusables()[0];
    first?.focus();
    window.addEventListener("keydown", keydown);
    return () => {
      window.removeEventListener("keydown", keydown);
      stack.splice(stack.indexOf(me), 1);
      undoInert();
      if (before && before.isConnected) before.focus();
    };
  });
</script>

<div class="backdrop" role="presentation" bind:this={backdrop}>
  <div class="sheet" role="dialog" aria-modal="true" aria-labelledby={id} style:width="{width}px" bind:this={sheet}>
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
