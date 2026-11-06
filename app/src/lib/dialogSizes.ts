// Each dialog's size as the user left it, by dialog id. Loaded from the
// settings when the app starts; a resize is remembered here at once and
// stored by the backend (which validates it). Without `initDialogSizes`
// (e.g. a component test) sizes live only as long as the page.
import type { DialogSize } from "./types";

export type DialogSizeSaver = (id: string, size: DialogSize) => Promise<void>;

let sizes: Record<string, DialogSize> = {};
let saver: DialogSizeSaver | null = null;

/** Start from the stored sizes; `save` stores a changed one. */
export function initDialogSizes(stored: Record<string, DialogSize> | null | undefined, save: DialogSizeSaver | null): void {
  sizes = { ...(stored ?? {}) };
  saver = save;
}

export function storedDialogSize(id: string): DialogSize | null {
  return sizes[id] ?? null;
}

/** Remember `size` for dialog `id` (a failure to store is not an error
 *  worth showing: the dialog just opens at its default size next time). */
export function rememberDialogSize(id: string, size: DialogSize): void {
  sizes[id] = size;
  saver?.(id, size).catch(() => {});
}
