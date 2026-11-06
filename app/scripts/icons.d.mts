export const ICONS: ReadonlyArray<readonly [name: string, size: number, svg: string]>;
export function render(svg: string, size: number): Promise<Uint8Array>;
export function committed(name: string): Uint8Array;
export function renderAll(): Promise<Array<[name: string, png: Uint8Array]>>;
