import type { InterfaceView } from "./types";

/** Only wired Ethernet can network-boot a RouterBOARD. */
export function suitable(i: InterfaceView): boolean {
  return i.kind === "ethernet";
}

/** A real Ethernet adapter (USB, PCI, built-in), not a Thunderbolt port. */
export function adapter(i: InterfaceView): boolean {
  return suitable(i) && !i.thunderbolt;
}

/** Ethernet adapters first, then other kinds, Thunderbolt ports last
 *  (the given order within each group). */
export function orderInterfaces(list: InterfaceView[]): InterfaceView[] {
  const rank = (i: InterfaceView) => (adapter(i) ? 0 : i.thunderbolt ? 2 : 1);
  return [...list].sort((a, b) => rank(a) - rank(b));
}

/** The picker's list, ordered: Ethernet only unless `all` (the selected one
 *  is always kept, so a remembered choice does not vanish). */
export function visibleInterfaces(
  list: InterfaceView[],
  all: boolean,
  selected: string | null,
): InterfaceView[] {
  return orderInterfaces(list.filter((i) => all || suitable(i) || i.name === selected));
}

/** What to select: the last used interface if it is listed, else the first
 *  Ethernet adapter with an IPv4 address, else the first adapter, else
 *  (only Thunderbolt ports) the first Ethernet-kind interface. */
export function defaultInterface(list: InterfaceView[], last: string | null): string | null {
  if (last && list.some((i) => i.name === last)) return last;
  const ordered = orderInterfaces(list);
  const pick =
    ordered.find((i) => adapter(i) && i.ipv4.length > 0) ?? ordered.find(adapter) ?? ordered.find(suitable);
  return pick?.name ?? null;
}

/** `192.168.88.1/24`, or `null` without IPv4. */
export function addressText(i: InterfaceView): string | null {
  const a = i.ipv4[0];
  return a ? `${a.address}/${a.prefix}` : null;
}
