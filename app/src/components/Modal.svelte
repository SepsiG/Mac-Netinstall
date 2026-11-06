<script lang="ts" module>
  // Open modals, innermost last: only the topmost one handles keys.
  const stack: symbol[] = [];
</script>

<script lang="ts">
  // A sheet-like modal dialog. While open: focus starts on the element
  // marked `data-autofocus` (the safe choice; else the first control), Tab
  // stays inside, Escape (anywhere in the window) calls `onclose`, and the
  // rest of the window is `inert`. Focus goes back where it was on close.
  //
  // The user can resize it with the grip in the bottom-right corner (pointer
  // events; the sheet stays centred, so its width changes by twice the
  // horizontal drag). It never gets smaller than `minWidth` × `minHeight`
  // (the content scrolls instead) nor larger than the window. By default
  // its width scales with the window (`width` at the default window width)
  // and its height follows the content; with an `id`, the size the user
  // leaves it at is remembered (see `dialogSizes`).
  import { onMount, untrack, type Snippet } from "svelte";
  import { rememberDialogSize, storedDialogSize } from "../lib/dialogSizes";

  let {
    title,
    onclose,
    children,
    actions,
    width = 440,
    id = null,
    minWidth = 320,
    minHeight = 160,
  }: {
    title: string;
    onclose: () => void;
    children: Snippet;
    actions?: Snippet;
    /** Default width at the default window width (it scales with the window). */
    width?: number;
    /** Remember the size under this dialog id. */
    id?: string | null;
    minWidth?: number;
    minHeight?: number;
  } = $props();

  /** The window width `width` is meant for (tauri.conf.json). */
  const BASE_WINDOW_WIDTH = 1040;
  /** Room kept around the sheet (horizontal: both sides; vertical: the
   *  backdrop's top padding and a bottom margin). */
  const MARGIN_X = 32;
  const MARGIN_Y = 64;

  let viewport = $state({ w: window.innerWidth, h: window.innerHeight });
  const stored = untrack(() => (id ? storedDialogSize(id) : null));
  /** `null`: the default (width scaled with the window; height from the content). */
  let size = $state<{ w: number | null; h: number | null }>({
    w: stored?.width ?? null,
    h: stored?.height ?? null,
  });

  const clamp = (v: number, lo: number, hi: number) => Math.min(Math.max(v, lo), Math.max(lo, hi));
  const maxW = $derived(viewport.w - MARGIN_X);
  const maxH = $derived(viewport.h - MARGIN_Y);
  const shownW = $derived(
    Math.round(clamp(size.w ?? (width * viewport.w) / BASE_WINDOW_WIDTH, minWidth, maxW)),
  );
  const shownH = $derived(size.h === null ? null : Math.round(clamp(size.h, minHeight, maxH)));

  let drag: { x: number; y: number; w: number; h: number } | null = null;

  function startResize(e: PointerEvent) {
    if (e.button !== 0) return;
    e.preventDefault();
    const h = shownH ?? (sheet.getBoundingClientRect().height || minHeight);
    drag = { x: e.clientX, y: e.clientY, w: shownW, h };
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
  }

  function moveResize(e: PointerEvent) {
    if (!drag) return;
    size = {
      w: clamp(drag.w + 2 * (e.clientX - drag.x), minWidth, maxW),
      h: clamp(drag.h + (e.clientY - drag.y), minHeight, maxH),
    };
  }

  function endResize() {
    if (!drag) return;
    drag = null;
    if (id && shownH !== null) rememberDialogSize(id, { width: shownW, height: shownH });
  }

  function viewportChanged() {
    viewport = { w: window.innerWidth, h: window.innerHeight };
  }

  const titleId = `modal-${Math.random().toString(36).slice(2)}`;
  const me = Symbol(titleId);
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
    window.addEventListener("resize", viewportChanged);
    return () => {
      window.removeEventListener("keydown", keydown);
      window.removeEventListener("resize", viewportChanged);
      stack.splice(stack.indexOf(me), 1);
      undoInert();
      if (before && before.isConnected) before.focus();
    };
  });
</script>

<div class="backdrop" role="presentation" bind:this={backdrop}>
  <div
    class="sheet"
    role="dialog"
    aria-modal="true"
    aria-labelledby={titleId}
    style:width="{shownW}px"
    style:height={shownH === null ? undefined : `${shownH}px`}
    style:min-width="{minWidth}px"
    style:min-height="{minHeight}px"
    style:max-height="{Math.max(minHeight, maxH)}px"
    bind:this={sheet}
  >
    <h2 id={titleId}>{title}</h2>
    <div class="body">{@render children()}</div>
    {#if actions}
      <div class="actions">{@render actions()}</div>
    {/if}
    <div
      class="grip"
      data-testid="resize-handle"
      aria-hidden="true"
      onpointerdown={startResize}
      onpointermove={moveResize}
      onpointerup={endResize}
      onpointercancel={endResize}
      onlostpointercapture={endResize}
    ></div>
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
    position: relative;
    display: flex;
    flex-direction: column;
    box-sizing: border-box;
    overflow: hidden;
    padding: 16px 18px 14px;
    border-radius: 10px;
    background: var(--panel);
    box-shadow: var(--shadow);
    border: 1px solid var(--separator);
  }
  h2 {
    flex: none;
    margin: 0 0 10px;
    font-size: 13px;
    font-weight: 600;
  }
  .body {
    flex: 1 1 auto;
    min-height: 0;
    overflow: auto;
  }
  .actions {
    flex: none;
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 14px;
  }
  /* The resize grip: three diagonal lines in the bottom-right corner. */
  .grip {
    position: absolute;
    right: 2px;
    bottom: 2px;
    width: 14px;
    height: 14px;
    cursor: nwse-resize;
    touch-action: none;
    background: linear-gradient(
      135deg,
      transparent 0 45%,
      var(--secondary) 45% 52%,
      transparent 52% 64%,
      var(--secondary) 64% 71%,
      transparent 71% 83%,
      var(--secondary) 83% 90%,
      transparent 90%
    );
    opacity: 0.6;
  }
  .grip:hover {
    opacity: 1;
  }
</style>
