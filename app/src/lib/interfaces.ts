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

/** The manual address macOS is configured with, as `172.30.0.2/22`. */
export function manualText(i: InterfaceView): string | null {
  const c = i.configured_ipv4;
  return c?.method === "manual" && c.address && c.prefix !== null ? `${c.address}/${c.prefix}` : null;
}

/** `192.168.88.1/24`: the live address, else the configured manual one
 *  (macOS hides it while there is no cable); `null` without either. */
export function addressText(i: InterfaceView): string | null {
  const a = i.ipv4[0];
  return a ? `${a.address}/${a.prefix}` : manualText(i);
}

/** There is a link: the interface has its address (macOS removes a
 *  manual one while there is no carrier). */
export function hasLink(i: InterfaceView): boolean {
  const manual = i.configured_ipv4?.method === "manual" ? i.configured_ipv4.address : null;
  return manual ? i.ipv4.some((a) => a.address === manual) : i.ipv4.length > 0;
}
