import type { InterfaceView } from "./types";

/** Only wired Ethernet can network-boot a RouterBOARD. */
export function suitable(i: InterfaceView): boolean {
  return i.kind === "ethernet";
}

/** The picker's list: Ethernet only unless `all` (the selected one is
 *  always kept, so a remembered choice does not vanish). */
export function visibleInterfaces(
  list: InterfaceView[],
  all: boolean,
  selected: string | null,
): InterfaceView[] {
  return list.filter((i) => all || suitable(i) || i.name === selected);
}

/** `192.168.88.1/24`, or `null` without IPv4. */
export function addressText(i: InterfaceView): string | null {
  const a = i.ipv4[0];
  return a ? `${a.address}/${a.prefix}` : null;
}
