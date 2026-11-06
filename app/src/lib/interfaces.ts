import type { InterfaceView } from "./types";

/** Only real wired Ethernet NICs (built-in, USB, Thunderbolt or dock
 *  adapters) can netinstall a RouterBOARD; Rust decides (`ni-net`). */
export function capableInterfaces(list: InterfaceView[]): InterfaceView[] {
  return list.filter((i) => i.netinstall_capable);
}

/** What to select: the last used interface if it is still capable, else
 *  the first capable one with an IPv4 address, else the first capable one. */
export function defaultInterface(list: InterfaceView[], last: string | null): string | null {
  const capable = capableInterfaces(list);
  if (last && capable.some((i) => i.name === last)) return last;
  return (capable.find((i) => i.ipv4.length > 0) ?? capable[0])?.name ?? null;
}

/** `192.168.88.1/24`, or `null` without IPv4. */
export function addressText(i: InterfaceView): string | null {
  const a = i.ipv4[0];
  return a ? `${a.address}/${a.prefix}` : null;
}
