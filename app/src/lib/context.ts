// The backend is shared through Svelte context, so tests can render any
// component with a mock (`render(C, { context: backendContext(mock) })`).
import { getContext, setContext } from "svelte";
import type { Backend } from "./api";

const KEY = Symbol("backend");

export function setBackend(backend: Backend): void {
  setContext(KEY, backend);
}

export function useBackend(): Backend {
  const b = getContext<Backend | undefined>(KEY);
  if (!b) throw new Error("no backend in context");
  return b;
}

/** A context map for testing-library's `render`. */
export function backendContext(backend: Backend): Map<symbol, Backend> {
  return new Map([[KEY, backend]]);
}
