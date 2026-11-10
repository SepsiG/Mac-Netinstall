# RouterOS v6 downloads and the version list (2026-11-10)

Verified on 2026-11-10 with HEAD/GET/range requests against MikroTik's
servers and by dumping downloaded packages (nothing was executed). These
facts drive v6 support, the version picker and the "local `.npk`" install in
v0.1.2.

## 1. Which version is the newest v6

- `https://upgrade.mikrotik.com/routeros/NEWEST6.long-term` →
  `6.49.23 <unix timestamp>` (same format as `NEWESTa7.<channel>`).
  `NEWEST6.stable` and `LATEST.6` answer the same 6.49.23.
- Do **not** use `NEWEST6.testing` or `LATEST.6rc`: both are stale and answer
  `7.12.1`.
- The v7 channels stay `NEWESTa7.{stable,long-term,testing,development}`.

## 2. v6 file names

Base: `https://download.mikrotik.com/routeros/<version>/`.

| What | v6 | v7 |
|---|---|---|
| Main package | `routeros-<arch6>-<v>.npk` (arch **before** the version) | `routeros-<v>-<arch>.npk` |
| Extra package | `<name>-<v>-<arch>.npk` | same |
| All packages | `all_packages-<arch>-<v>.zip` | same |
| Checksum | `<file>.sha256` from **6.49.13** on | from 7.14.3 / required from 7.15 |

- `arch6` is `powerpc` for the `ppc` architecture and the arch name itself
  otherwise (`routeros-powerpc-6.49.23.npk`, `routeros-mipsbe-6.49.23.npk`).
  Extras and the zip keep `ppc` (`calea-6.49.23-ppc.npk`,
  `all_packages-ppc-6.49.23.zip`).
- The v7 order (`routeros-6.49.23-mipsbe.npk`) answers 404.
- The zip's central directory can be read with a ranged request
  (`Range: bytes=-65536`), exactly as for v7. The 6.49.23 zips list
  `system`, `ipv6`, `wireless`, `hotspot`, `mpls`, `routing`, `ppp`, `dhcp`,
  `security`, `advanced-tools` (the parts of the bundle, see below) plus the
  real extras: `calea`, `gps`, `iot`, `lcd`, `lora`, `lte`, `multicast`,
  `ntp`, `openflow`, `tr069-client`, `ups`, `user-manager` (per arch; smips
  has fewer).

## 3. The v6 main package is a bundle

`routeros-smips-6.49.23.npk` (7 554 171 bytes, published SHA-256
`2dcf2fcc22b458d42eef9a086e8a3d1c1bfa0dedac6548d0c1f597551ff0daf5`) consists of:

```
25 (empty)
18 "routeros-smips\0\0" + version bytes 17 66 31 06
24 "stable"
16 "smips"
1  "system"          + version, 16 "smips", ... (the system package's parts)
1  "ipv6"            + 16 "smips", ...
1  "wireless@"       + 16 "smips", ...   (name not NUL-terminated cleanly)
1  "hotspot", "mpls", "routing", "ppp", "dhcp", "security", "advanced-tools"
16 "I"
```

- The sub-packages are flat parts of the one file (no nested NPK).
- The existing header reader (first type-1 part, then the first type-16 part
  after it) reads `system` and the arch unchanged.
- A sub-package name may carry junk after the name (`wireless@`): take the
  prefix up to the first NUL or non-name byte.
- Installing an extra package that is already in the bundle would install it
  twice, so the v6 extra-package list excludes the bundle's names.

## 4. Architecture strings inside the packages

Identical for v6 and v7, and equal to the device's Netinstall hello arch:
`mipsbe` → `mips`, `ppc` → `powerpc`, `smips` → `smips`, `mmips`, `arm`,
`arm64`, `tile`. The NPK-arch verification before formatting works for v6
without changes.

## 5. Version bytes in the package header

Part type 1 (and the bundle's type 18): 16 bytes of NUL-padded name, then
4 version bytes `patch, kind, minor, major`:

| File | bytes | version |
|---|---|---|
| routeros-7.12.1-mipsbe.npk | `01 66 0c 07` | 7.12.1 |
| routeros-mipsbe-6.49.23.npk | `17 66 31 06` | 6.49.23 |
| calea-6.49.23-arm.npk | `17 66 31 06` | 6.49.23 |

`kind` `0x66` (`f`) is a final release. Other kinds (betas, rcs) are not
decoded: a local package of such a version is shown as "local".

## 6. Availability of old versions

- `mipsle` exists only up to 6.3x: not supported (out of scope).
- 6.30.4 has no `arm` and no `mmips` packages; `arm64` exists from about
  6.45. A missing package answers 404 → reported as
  `package_not_published` (arch, version).
- Versions older than 6.30 are refused (`version_too_old`).

## 7. Boot kernels for v6 installs

- The protocol we speak is the v7 one. The community confirms that the 7.x
  netinstall installs 6.49.x (e.g. on a hAP lite).
- So v6 installs (and installs from local `.npk` files) boot the **current
  7.x netinstall kernels** (stable channel). The kernels inside the v6
  `netinstall-6.49.23.tar.gz` (21 MB) are not used.
- The mode script (`mode.scr`) needs RouterOS 7.22 or newer: profiles for
  older versions cannot use it (`mode_script_needs_7_22`).

## 8. Version list sources

- Channel heads (reliable, tiny): `NEWESTa7.{stable,long-term,testing,
  development}` and `NEWEST6.long-term`.
- Full history: only the changelog page
  `https://mikrotik.com/download/changelogs?channelFilter=stable` and
  `...?channelFilter=longTerm` (HTML, 0.4–1.4 MB, `Cache-Control: no-cache`).
  Each release is one element with `data-changelog-version="X"`, newest
  first; its header holds the release date as text (`2026-11-02`). The
  `channelFilter` parameter is undocumented: the scrape is fragile, so the
  app keeps the last good list and a built-in seed.
- On 2026-11-10 the stable page listed 7.25.1 … 7.1, 6.49.23 … 6.0 and some
  5.x/4.x/3.x; the long-term page 7.24.6 … 7.20.7 and 6.49.23 … 6.30.1.
- Unusable: `https://download.mikrotik.com/routeros/` (403) and
  `https://mikrotik.com/download/archive` (no machine-readable list).
- Existence check for a typed version: `HEAD
  https://download.mikrotik.com/routeros/<v>/CHANGELOG` (200 for a published
  version, 404 otherwise).

## 9. Risk: installing below the device's minimum version

Installing a version older than the device's factory (minimum) version can
leave it unbootable (MikroTik manual, "Downgrading"). The Netinstall hello's
field 5 (`min_os`) is the device's minimum RouterOS version. The app refuses
a version below it before formatting (`version_below_minimum`), unless the
profile explicitly allows it (advanced option, off by default). The official
netinstall-cli does not check this.
