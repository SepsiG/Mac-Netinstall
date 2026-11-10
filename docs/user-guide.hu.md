# Mac Netinstall – felhasználói útmutató

*English version: [user-guide.en.md](user-guide.en.md)*

> **Állapot:** valódi hardveren, egy **hEX RB750Gr3**-on (`mmips`, RouterOS 7.25.1)
> kipróbálva: mind a négy konfigurációs mód (alapértelmezett, üres, egyéni szkript,
> meglévő megtartása) és az automatikus ismétlés működik; egy **hAP ac²** (`arm`) is
> felment egy extra csomaggal, és egy **CRS125-24G-1S-2HnD** (`mipsbe`) is. Az archívumból a
> RouterOS **6.49.23** is felment a hEX-re. A többi architektúrán (arm64, smips, ppc, tile), a módszkripttel, a márkajelzés eltávolításával, a licenckulccsal és a
> telepítés utáni leállítással **még nem tesztelt** hardveren. Ilyen típusoknál tarts kéznél
> tartalékot: olyan eszközt, amelyet szükség esetén a MikroTik hivatalos Netinstalljával
> (Windows/Linux) helyre tudsz állítani.
>
> **Felelősség kizárása:** a szoftvert „ahogy van” alapon, mindenféle garancia nélkül
> biztosítjuk. A Netinstall törli az eszközt: saját felelősségedre használd. A szerzők nem
> vállalnak felelősséget semmilyen kárért, azért sem, ha az eszköz használhatatlanná válik
> („téglásodás”). Részletek: [Felelősség kizárása](#13-felelősség-kizárása).

## Tartalom

1. [Mi ez?](#1-mi-ez)
2. [Követelmények](#2-követelmények)
3. [Telepítés](#3-telepítés)
4. [A Mac előkészítése](#4-a-mac-előkészítése)
5. [Profil létrehozása](#5-profil-létrehozása)
6. [Telepítés lépésről lépésre](#6-telepítés-lépésről-lépésre)
7. [Több eszköz és automatikus ismétlés](#7-több-eszköz-és-automatikus-ismétlés)
8. [Beállítások](#8-beállítások)
9. [Napló és hibaelhárítás](#9-napló-és-hibaelhárítás)
10. [Parancssor (ni-cli)](#10-parancssor-ni-cli)
11. [Ismert korlátok](#11-ismert-korlátok)
12. [Visszajelzés](#12-visszajelzés)
13. [Felelősség kizárása](#13-felelősség-kizárása)

## 1. Mi ez?

A Mac Netinstall egy macOS-alkalmazás (Apple Silicon), amellyel MikroTik RouterBOARD
eszközökre telepíthetsz újra RouterOS-t Netinstallal – arra az esetre, amikor a MikroTik
hivatalos Netinstall programja nem fut a Maceden. Nem a MikroTik terméke, és nem áll
kapcsolatban vele.

A Netinstall **leformázza az eszköz tárolóját, és tiszta RouterOS-t telepít** rá. A
fájlok, konténerek és egyéb felhasználói adatok elvesznek. Hogy a konfigurációból mi
marad meg, azt a profil konfigurációs módja dönti el (lásd [5. fejezet](#5-profil-létrehozása)).
A MikroTik leírása szerint a Netinstall a licenckulcsot és a RouterBOOT-beállításokat
nem törli.

Az alkalmazás a RouterOS boot kerneleit és csomagjait futás közben tölti le a
mikrotik.com-ról, és helyben tárolja őket; MikroTik-binárisokat nem tartalmaz.

## 2. Követelmények

- **Mac** Apple Silicon processzorral (M1 vagy újabb), **macOS 13** vagy újabb. Intel
  Macen nem fut.
- **Vezetékes Ethernet-csatoló**: beépített port, USB- vagy Thunderbolt-adapter, illetve
  dokkoló Ethernet-portja. A Wi-Fi és a Thunderbolt-hídként (IP Thunderbolton) működő
  portok nem használhatók, ezért az alkalmazás meg sem jeleníti őket.
- **Ethernet-kábel** a Mac és a RouterBOARD között. Legjobb, ha az adapter csak a
  routerhez csatlakozik (nem a céges hálózathoz).
- **Internet** az első letöltéshez. A letöltött boot kernelek és csomagok a Macen
  maradnak, így később offline is lehet telepíteni ugyanazt a verziót.
- Maga a **RouterBOARD** és a tápellátása. Az x86 és ARM64 UEFI eszközök nem
  támogatottak.

## 3. Telepítés

1. A [Releases](https://github.com/SepsiG/Mac-Netinstall/releases) oldalról töltsd le
   a `Mac-Netinstall_<verzió>_aarch64.dmg` és a `Mac-Netinstall_<verzió>_aarch64.dmg.sha256`
   fájlt.
2. (Ajánlott) Ellenőrizd a letöltést. A Terminálban, abban a mappában, ahol mindkét
   fájl van:

   ```sh
   shasum -a 256 -c Mac-Netinstall_<verzió>_aarch64.dmg.sha256
   ```

   Az eredmény: `Mac-Netinstall_<verzió>_aarch64.dmg: OK`.
3. Nyisd meg a `.dmg` fájlt, és húzd a **Mac Netinstall** ikont az **Alkalmazások**
   (Applications) mappára.
4. **Első indítás.** Az alkalmazás ad-hoc aláírású, nincs az Apple-nél hitelesítve
   (notarizálva), ezért a macOS elsőre letiltja.
   - **macOS 15 és újabb:** próbáld elindítani, majd nyisd meg a Rendszerbeállítások →
     Adatvédelem és biztonság oldalt, görgess le a „Mac Netinstall”-ra vonatkozó
     üzenetig, és kattints a mellette lévő megnyitás gombra (angol rendszeren
     **Open Anyway**), végül erősítsd meg.
   - **macOS 13–14:** Control-kattintás az alkalmazásra → Megnyitás is működik.
5. **Engedélyek.** Az első indításkor megnyílik a **macOS-engedélyek** ablak. Kattints az
   **Engedélyek kérése** gombra: a macOS azonnal rákérdez (nem telepítés közben)
   - hogy a Mac Netinstall kereshet-e és csatlakozhat-e eszközökhöz a helyi hálózaton
     (**Helyi hálózat**) – **engedélyezd**, enélkül az alkalmazás nem tud válaszolni az
     eszközöknek;
   - ha be van kapcsolva a macOS tűzfala, hogy fogadhat-e bejövő kapcsolatokat –
     **engedélyezd**.

   Az ablak ezután mutatja, mit válaszolt a macOS (✓ / ✗), és ami nincs engedélyezve,
   ahhoz megnyitja a Rendszerbeállítások megfelelő oldalát (Adatvédelem és biztonság →
   Helyi hálózat, illetve a tűzfal). Ha módosítottál egy beállítást, kattints az
   **Újraellenőrzés** gombra. A **Később** bezárja az ablakot; a következő indításkor újra
   megnyílik, amíg nem kérted az engedélyeket. Bármikor megnyithatod a **Beállítások →
   Engedélyek ellenőrzése** gombbal is.

Futás közben az alkalmazás a Dockban van; a Dock-ikon jelvénye mutatja, hány eszköz áll
éppen előkészítés vagy telepítés alatt.

## 4. A Mac előkészítése

Az alkalmazás **nem módosítja** a Mac hálózati beállításait (így rendszergazdai jog sem
kell hozzá). A szerver címe az Ethernet-adapter macOS-ben **kézzel beállított IPv4-címe**,
ezt neked kell megadnod:

1. Dugd be az Ethernet-adaptert.
2. Nyisd meg: Rendszerbeállítások → Hálózat → az adapter → Részletek → TCP/IP.
3. IPv4 konfigurálása: Kézzel, IP-cím `192.168.88.1`, alhálózati maszk `255.255.255.0`,
   majd OK.
4. Ha be van kapcsolva az **Internetmegosztás** (Rendszerbeállítások → Általános →
   Megosztás), kapcsold ki: a saját DHCP-szervere ütközik az alkalmazással. Ugyanez
   érvényes minden más DHCP- vagy TFTP-szerverre a Macen.

Más alhálózat is jó (legalább /30-as), a `192.168.88.1/24` csak a megszokott példa.

### Amit az alkalmazás mutat

A főablak tetején a **Hálózati csatoló** listában csak a használható vezetékes
Ethernet-csatolók jelennek meg (név, megjelenített név, aktuális IPv4-cím). Mellette
látod a kiválasztott adapter állapotát:

| Kijelzés | Jelentése |
|---|---|
| **Kézi: 192.168.88.1/24** (a te címeddel) | Rendben, kézi IPv4 van beállítva. |
| **DHCP – állítsd kézire** | Az adapter DHCP-vel kap címet; állítsd kézire (lásd fent). |
| **Nincs IPv4 beállítva** | Nincs cím; állíts be kézi IPv4-et. |
| **van kapcsolat** | Van link (kábel bedugva, a másik oldal be van kapcsolva). |
| **nincs link – nincs bedugva a kábel?** | Nincs link. Indítani így is lehet, a kiszolgálás a link megjelenésekor indul. |

A ↻ gomb (**Csatolólista frissítése**) újraolvassa a csatolókat. Leállított állapotban
az alkalmazás néhány másodpercenként magától is újraellenőrzi a kábelt és a címet.

Ha egyáltalán nincs megfelelő adapter, ezt látod:
**Nincs használható vezetékes Ethernet-csatoló. Csatlakoztass egy USB vagy Thunderbolt Ethernet-adaptert.**

Az eszköztár alatti állapotsor mutatja az indítás előtti ellenőrzés (preflight) eredményét.
Ha minden rendben, ezt írja: **✓ Indításra kész**. Ha valami hiányzik, a **Teendő:**
sorok pontosan leírják, mit kell beállítani; ha ennek ellenére megnyomod az **Indítás**
gombot, ezek pirosan, hibaként jelennek meg, és az indítás nem történik meg.

## 5. Profil létrehozása

A profil írja le, mit telepítsen az alkalmazás. A főablak **Profil** sorában válaszd ki,
vagy hozz létre újat az **Új…** gombbal; a meglévőt a **Szerkesztés…** gombbal
módosíthatod. A profillista elemei „név · verzió” formában jelennek meg.

| Mező | Leírás |
|---|---|
| **Név** | 1–64 karakter: betű, szám, szóköz vagy `. _ - +`; nem kezdődhet ponttal vagy szóközzel. |
| **Csomagok forrása** | **Letöltés a MikroTiktől** (alapértelmezett) vagy **Saját csomagfájlok (.npk)** (lásd [Telepítés saját .npk fájlokból](#telepítés-saját-npk-fájlokból)). |
| **RouterOS-verzió** | A verzióválasztóban választod ki (lásd [A verzió kiválasztása](#a-verzió-kiválasztása)): olyan csatornát, amely mindig az aktuális verzióját telepíti, egy konkrét RouterOS 7 vagy archív 6.x verziót, vagy kézzel beírt verziót. |
| **További csomagok** | A `routeros` rendszercsomag mindig települ; itt jelölheted be a további csomagokat (pl. `wifi-qcom`). A **Lista ehhez** választóval nézheted meg, mely csomagok érhetők el egy adott architektúrára – ez csak a lista szűrése, az eszköz architektúráját az alkalmazás magától felismeri. |
| **Konfiguráció** | Mi legyen a konfigurációval (lásd alább). |
| **Telepítés után** | **Újraindítás**, **Újraindítás „Installation successful” üzenettel** vagy **Leállítás**. |
| **Haladó** | Módszkript, márkajelzés eltávolítása, licenckulcs, telepítés az eszköz minimális verziója alá (lásd alább). |

### A verzió kiválasztása

A **RouterOS-verzió** lista három csoportból áll, és kereshető (**Verzió keresése**):

- **Mindig a legújabb** – **Stabil**, **Hosszú távú**, **Teszt**, **Fejlesztői** és
  **RouterOS v6 (hosszú távú)**, mindegyik mellett zárójelben az a verzió, amelyet épp
  jelent. A csatornát használó profil mindig a csatorna aktuális verzióját telepíti.
- **RouterOS 7** – konkrét kiadások a MikroTik változásnaplójából, a legújabbal kezdve,
  csatornával (Stabil, Hosszú távú) és megjelenési dátummal.
- **Archív (6.x, már nem támogatott)** – RouterOS 6 kiadások 6.30-tól.
- **Egyéni verzió…** – írj be egy verziót (pl. `7.20.8` vagy `6.49.23`); az alkalmazás
  ellenőrzi, hogy a download.mikrotik.com közzéteszi-e (**elérhető** / **nincs a
  download.mikrotik.com-on**).

A lista a mikrotik.com-ról jön, és egy napig érvényes; alatta látszik, milyen régi, a
**Frissítés** gomb újra letölti. Internet nélkül az utolsó lista (vagy egy rövid beépített
lista) látszik.

### RouterOS 6 (archív verziók)

RouterOS 6 (6.30-tól 6.49.x-ig) is telepíthető. Amit érdemes tudni:

- **A RouterOS 6 életciklusa véget ért.** Csak akkor használd, ha okod van rá (pl. régi
  eszköz vagy v6-ot igénylő konfiguráció).
- **Soha ne telepíts az eszköz minimális (gyári) verziójánál régebbit.** Ettől az eszköz
  használhatatlanná válhat. Az eszköz a minimális verzióját a felismeréskor, a Netinstall
  bejelentkezésében (hello) küldi el (a naplósor és a típus melletti buborék mutatja; például
  egy hEX RB750Gr3 6.36.1-et, egy LTE-s hAP ac² (D53G-5HacD2HnD&FG621-EA) 7.11-et jelez); az
  alkalmazás a régebbit **megtagadja**, mielőtt bármit törölne: **„A RouterOS …
  régebbi, mint az eszköz minimális verziója …”**. Ha valamelyik verzió nem olvasható ki
  (pl. ismeretlen verziójú saját csomag), a telepítést szintén megtagadja. Egyes újabb
  eszközök a v6-ot egyáltalán nem támogatják.
- Régi kiadások nem minden architektúrára jelentek meg (pl. a 6.30.4-hez nincs `arm` és
  `mmips` csomag; `arm64` nagyjából 6.45-től van): **„A RouterOS … nem jelent meg …
  architektúrára.”**
- Az eszköz a jelenlegi RouterOS 7 netinstall kernelével indul, amely a v6 csomagokat is
  telepíti.
- A v6 fő csomagja magában foglalja a `system`, `ipv6`, `wireless`, `hotspot`, `mpls`,
  `routing`, `ppp`, `dhcp`, `security` és `advanced-tools` csomagot; a további csomagok
  listája csak a valódi kiegészítőket mutatja.
- A módszkripthez RouterOS 7.22 vagy újabb kell, ezért v6-tal nem használható.

Javaslat az első próbához: telepítsd a `6.49.23`-at egy hEX-re, amelyet helyre tudsz
állítani.

### Telepítés saját .npk fájlokból

A **Csomagok forrása** sorban válaszd a **Saját csomagfájlok (.npk)** lehetőséget, majd a
**Válassz .npk fájlokat…** gombot (egyszerre többet is kijelölhetsz). A lista mutatja
minden fájl csomagnevét, architektúráját és verzióját, ahogy a fájlból kiolvasható. A ×
gombbal eltávolíthatsz egyet.

- Az egyik fájlnak fő (`system`/`routeros`) csomagnak kell lennie; ez települ elsőként.
- Minden fájlnak az eszköz architektúrájára kell készülnie; ezt az eszköz formázása előtt
  ellenőrizzük.
- A telepített verziót a fő csomagból olvassuk ki („ismeretlen verzió”, ha nem
  olvasható; ekkor a módszkript nem használható).
- A boot kernelek ekkor is a jelenlegi stabil RouterOS 7 netinstallból jönnek: ehhez
  egyszer internetkapcsolat kell, vagy megtartott letöltések (lásd
  [Beállítások](#8-beállítások)).
- A fájlokat az alkalmazás a saját mappájába másolja; ha ott törlöd, válaszd ki újra.

### Konfigurációs módok

| Mód | Mi történik | Mikor használd |
|---|---|---|
| **Meglévő konfiguráció megtartása** | Az eszköz megtartja a jelenlegi beállításait. A MikroTik leírása szerint ez a konfigurációt és a felhasználókat állítja vissza; a felhasználói fájlok, konténerek nem maradnak meg. | Sérült vagy elfelejtett RouterOS frissítése/javítása, ha a beállítások jók. |
| **Gyári alapkonfiguráció** | Az első indításkor a RouterOS gyári alapkonfigurációja lép életbe. | Tiszta, „gyári” állapot. **Ez az alkalmazás alapértelmezése.** |
| **Üres konfiguráció** | Semmilyen konfiguráció nem lesz: az eszköz MAC-címen (WinBox) vagy konzolon érhető el. | Ha teljesen nulláról akarod beállítani. |
| **Saját szkript (.rsc)** | Az első indításkor a saját RouterOS-szkripted fut le a gyári alapkonfiguráció helyett. A **Kiválasztás…** gombbal választod ki a fájlt. | Sorozattelepítés egységes alapbeállítással. |

> **Figyelem:** a hivatalos `netinstall-cli` alapértelmezése a **megtartás**, ennek az
> alkalmazásnak viszont a **Gyári alapkonfiguráció**. Ha a beállításokat meg akarod
> tartani, ezt külön válaszd ki a profilban.

Üres szkriptfájlt nem fogad el az alkalmazás; konfiguráció nélküli telepítéshez az
**Üres konfiguráció** módot válaszd.

### Haladó beállítások

- **Módszkript (mode.scr)** – kiegészítő RouterOS-szkript, amely `mode.scr` néven kerül
  az eszközre (a hivatalos eszköz `-sm` kapcsolója). A MikroTik leírása szerint
  egyszer, az első indításkor fut a többi konfigurációs szkript előtt, és RouterOS 7.22
  vagy újabb kell hozzá. A meglévő konfiguráció megtartásával és 7.22 előtti verzióval
  (így RouterOS 6-tal sem) nem használható.
- **Márkajelzés (branding) eltávolítása** – a hivatalos eszköz `-b` kapcsolójának
  megfelelő jelzést küldi. Hogy az eszköz mit tesz vele, még nem ellenőriztük.
- **Licenckulcs:** – **Kulcsfájl kiválasztása…**. Csak szoftverkulcsot igénylő
  eszközökhöz (pl. x86) kell, a hivatalos eszköz `-k` kapcsolója. A RouterBOARD-ok
  licence az eszközön van, ezt a mezőt általában üresen hagyod. A kulcsot a profilfájl
  tárolja, amelyet csak a te felhasználód olvashat.
- **Telepítés az eszköz minimális verziója alá is (kockázatos)** – alapból kikapcsolva.
  Az eszköz által jelzett minimális verziónál régebbit az alkalmazás nem telepíti, és
  semmit sem töröl, mert az eszköz használhatatlanná válhat; akkor sem, ha a telepítendő
  verzió vagy az eszköz minimális verziója nem olvasható ki. Csak akkor kapcsold be, ha
  biztosan tudod, hogy az eszköz támogatja az adott verziót.

A **Mentés** elmenti, a **Mégse** bezárja a szerkesztőt (mentetlen módosításoknál
rákérdez). Meglévő profil a **Törlés…** gombbal törölhető.

## 6. Telepítés lépésről lépésre

1. Kösd össze kábellel a Mac Ethernet-adapterét és a RouterBOARD **boot-portját**
   (általában az **ether1**, vagy a „BOOT” feliratú port; kétség esetén lásd az eszköz
   kézikönyvét).
2. A **Hálózati csatoló** listában válaszd ki az adaptert, a **Profil** listában a
   profilt. Ha olyan eszközt telepítesz, amelyet az alkalmazás magától kezdjen el, kapcsold
   be az **Automatikus ismétlés** jelölőt (lásd [7. fejezet](#7-több-eszköz-és-automatikus-ismétlés)).
3. Várd meg, amíg az állapotsor ezt mutatja: **✓ Indításra kész**, majd kattints az
   **Indítás** gombra. Első alkalommal megjelenik a **Felelősség kizárása** ablak: jelöld
   be a **Megértettem, és saját felelősségemre használom** négyzetet, majd kattints az
   **Elfogadom** gombra (a **Mégse** semmit sem indít el). Ha kell, az alkalmazás ekkor tölti le a boot kerneleket. Az
   állapotsor jobb oldalán a **Boot kernelek:** felirat mutatja az állapotukat (pl.
   `RouterOS 7.25.1`, letöltés közben százalékkal). Futás közben a bal oldalon a
   **Kiszolgálás: en7 (192.168.88.1)** jellegű sor látszik.
4. Indítsd a RouterBOARD-ot **Etherboot (Netinstall) módban** – lásd lent.
5. Figyeld az eszköztáblát. Ha be van kapcsolva az **Automatikus ismétlés**, a telepítés
   magától elindul. Ha nincs, az eszköz **Profilra vár** állapotban áll meg: kattints a
   sorában a **Telepítés indítása** gombra (a kiválasztott profil települ).
6. Amikor az állapot **Kész (…)** lesz (pl. „Kész (2 perccel ezelőtt)”), a telepítés befejeződött,
   és az eszköz a profil szerint újraindul vagy leáll. Újraindulás után már a
   tárolójáról, az új RouterOS-szel indul.
7. A végén kattints a **Leállítás** gombra.

### Etherboot mód a RouterBOARD-on

A MikroTik hivatalos leírása:
[Netinstall – MikroTik dokumentáció](https://help.mikrotik.com/docs/spaces/ROS/pages/24805390/Netinstall).
Röviden:

- **Reset gombbal:** kapcsold ki az eszközt, nyomd le és tartsd nyomva a reset gombot,
  kapcsold be, és tartsd nyomva addig, amíg az eszköz meg nem jelenik az alkalmazás
  eszköztáblájában.
- **RouterOS-ből** (ha még elérhető): futtasd ezt, majd indítsd újra az eszközt:

  ```
  /system routerboard settings set boot-device=try-ethernet-once-then-nand
  ```

  Az eszköz egyszer megpróbál hálózatról indulni, utána a megszokott módon a saját
  tárolójáról.
- **Port:** az ether1 vagy a „BOOT” feliratú port. Ha egy eszköz nem jelenik meg,
  ellenőrizd, jó portba dugtad-e.
- Ha az eszközön védett bootloader (protected bootloader) van beállítva, a reset gomb
  viselkedése eltérhet; ezt a MikroTik dokumentációja részletezi.

### Az eszköztábla

Oszlopok: **MAC-cím**, **Típus**, **Arch.**, **Állapot**, **Folyamat**, **Verzió**,
**Műveletek**. A **Verzió** alatt a konfigurációs mód is megjelenik (pl. „gyári
alapkonfiguráció”). Ha egy eszköznél ez már nem az első próbálkozás, az állapot fölé
víve az egeret látod, hányadik (pl. „2. próbálkozás”).

| Állapot | Jelentése |
|---|---|
| **Indul** | Az eszköz hálózatról indul (BOOTP-kérés érkezett). |
| **Boot kernel küldése** | A boot kernel letöltése az eszközre (TFTP), folyamatjelzővel. |
| **Megtalálva** | A boot kernel elindult, az eszköz bejelentkezett (típus, architektúra). |
| **Profilra vár** | Nincs hozzárendelt profil: **Telepítés indítása** vagy **Automatikus ismétlés**. |
| **Szabad helyre vár** | Már annyi eszköz települ, amennyit a beállítás enged. |
| **Előkészítés** | A szükséges csomagok letöltése/ellenőrzése. |
| **Kezdés**, **Formázás**, **Feltöltés**, **Befejezés**, **Újraindítás** | A telepítés lépései; a **Folyamat** oszlop százalékot mutat. |
| **Kész (…)** | Sikeres telepítés, mellette, hogy mennyi ideje. |
| hibaüzenet (piros sor) | Sikertelen telepítés; a szöveg az okot írja le (lásd [9. fejezet](#9-napló-és-hibaelhárítás)). |

Ha leállítod a kiszolgálást, a tábla az utolsó ismert állapotot mutatja ezzel a
megjegyzéssel: **Utolsó ismert állapot; a kiszolgálás leállt.**

### Leállítás vagy kilépés telepítés közben

Ha telepítés közben nyomod meg a **Leállítás** gombot, vagy kilépsz az alkalmazásból,
megerősítést kér (**Leállítod a kiszolgálást?** / **Kilépsz a Mac Netinstallból?**).
Ennek oka: egy félig formázott vagy félig feltöltött eszköz működő rendszer nélkül
maradhat, és újra hálózatról kell indítani. Válaszd a **Folytatás** (illetve
**Mégse**) gombot, és várd meg a végét, hacsak nem biztos, hogy meg akarod szakítani.

## 7. Több eszköz és automatikus ismétlés

- **Automatikus ismétlés** (az eszköztáron, csak kiválasztott profillal): minden új
  eszközre a kiválasztott profilt telepíti, kérdés nélkül. Sorozattelepítéshez: egymás
  után indítsd Etherboot módban az eszközöket.
- **Egyszerre telepített eszközök** (Beállítások): alapból **1**, azaz egyszerre egy
  eszköz települ, a többi **Szabad helyre vár**. Legfeljebb 64-re emelheted; a boot
  kernel és a csomagok ugyanazon a kábelen osztoznak.
- Várakozási idő telepítés után (Beállítások → „Telepítés után ennyi ideig figyelmen
  kívül hagyja (másodperc)”): alapból 300 másodperc (5 perc), legalább 30. Ennyi ideig
  az épp telepített eszközt figyelmen kívül hagyja, hogy újraindulás közben ne telepítse
  újra. Utána is csak akkor foglalkozik vele ismét, ha az eszköz újra hálózatról indul.
- **Csak ezt** / **Mindet** (sorban, futás közben): a **Csak ezt** gombbal csak ezt az
  egy eszközt szolgálja ki (az állapotsorban: „Csak ezt az eszközt szolgálja ki: …”);
  a **Mindet** visszaállítja, hogy minden eszközt kiszolgáljon. Hasznos, ha több
  router is ugyanazon a hálózaton van.
- **Újra** – sikertelen telepítés újrapróbálása.
- **Megszakítás** – előkészítés vagy telepítés megszakítása, illetve sorban álló eszköz
  kivétele. Mindig rákérdez; telepítés közben figyelmeztet, hogy az eszköz működő
  rendszer nélkül maradhat (**Telepítés leállítása** / **Folytatás**).
- **Elfelejtés** – eltávolítja az eszközt a táblából, így újra felismerhető. A
  várakozási idő alatt nem lehet elfelejteni egy épp telepített eszközt (a gomb
  ilyenkor szürke, a tippje: „Épp most települt és újraindul: a várakozási idő letelte
  után lehet elfelejteni.”).
- **Dock:** a Dock-ikon jelvénye mutatja az előkészítés vagy telepítés alatt álló
  eszközök számát; ha egy eszköz a háttérben végez, az ikon egyszer ugrik.

## 8. Beállítások

Az eszköztár **Beállítások** gombjával nyílik.

- **Nyelv** – **A macOS nyelve**, **English** vagy **Magyar**.
- **Engedélyek** – az **Engedélyek ellenőrzése** megnyitja a macOS-engedélyek ablakát
  (lásd a [3. fejezetet](#3-telepítés)).
- **Telepítés** – **Egyszerre telepített eszközök** és a várakozási idő (lásd
  [7. fejezet](#7-több-eszköz-és-automatikus-ismétlés)).
- **Haladó**
  - **Címtartomány** – a hálózatról induló eszközöknek kiosztott címek. Alapból
    automatikus: az adapter alhálózata a saját címe nélkül (a párbeszédablak mutatja a
    kiszámolt tartományt). Az **Egyéni tartomány** gombbal saját tartományt adhatsz meg
    az alhálózaton belül (nem lehet az első vagy utolsó cím); a **Vissza az
    automatikusra** visszaállítja.
  - **Letöltési időkorlát (perc)** – alapból 30 perc; csak nagyon lassú kapcsolatnál
    kell növelni.
- **Letöltött RouterOS fájlok** – a mikrotik.com-ról letöltött boot kernelek és
  csomagok listája verziónként, mérettel és helyükkel
  (`~/Library/Caches/io.github.sepsig.mac-netinstall`). A fájlok állapota:

  | Jelzés | Jelentése |
  |---|---|
  | **✓ ellenőrizve** | Egyezik a közzétett ellenőrzőösszeggel. |
  | **nincs közzétett ellenőrzőösszeg** | A MikroTik nem közöl hozzá ellenőrzőösszeget. |
  | **még nincs ellenőrizve** | A **Fájlok ellenőrzése** gombbal ellenőrizheted. |
  | **sérült** / **hiányos** | Töröld (újra letöltődik, ha kell). |
  | **befejezetlen** | Megszakadt letöltés. |

  Törölhetsz egy fájlt (×), egy verziót (**Verzió törlése**), a sérült fájlokat
  (**Sérült fájlok törlése**) vagy mindent (**Összes törlése**); mindegyik rákérdez.
  A **Saját fájlok (szkriptek, csomagok)** csoportban a profilokhoz választott
  szkriptek vannak; ha törlöd őket, a profilban újra ki kell választani a fájlt. Épp
  használt verziót nem lehet törölni: **„A RouterOS … épp használatban van (kiszolgálás
  vagy telepítés alatt). Előbb állítsd le, vagy más fájlt törölj.”**

- **Letöltött fájlok megtartása kilépés után** (a **Letöltött RouterOS fájlok** alatt) –
  alapból kikapcsolva: a letöltött kernelek és csomagok **kilépéskor törlődnek** (a
  maradékuk a következő indításkor). A saját fájljaidat (szkriptek, csomagok) és a
  verziólistákat mindig megtartjuk. Kapcsold be, ha internet nélkül dolgozol, vagy több
  napon át sok eszközt telepítesz: megtartott fájlok nélkül internet nélkül nem lehet az
  utoljára használt verzióra visszaállni. **Frissítés a 0.1.1-ről:** meglévő
  beállításfájl esetén a letöltések megmaradnak (a négyzet be van jelölve); ha törölni
  szeretnéd őket, vedd ki a jelet. A gyorsítótárat az `ni-cli` is használja, ezért a
  beállítás az általa letöltött fájlokat is törli; azt a verziót, amelyet épp egy másik
  program használ, kihagyja.
- **Névjegy** – az alkalmazás verziója, licence (GNU GPL v3), a felelősségkizárás, és
  hivatkozások a GitHub-oldalra, a teljes felelősségkizárásra és a licencre.

A **Mentés** gomb menti a beállításokat, a **Mégse** elveti a módosításokat.

## 9. Napló és hibaelhárítás

### Napló

Az ablak alján lévő **Napló** panel lenyitható; a legújabb sor van felül. A fölötte lévő
elválasztó vonal húzásával magasabbra vagy alacsonyabbra állíthatod (vagy jelöld ki, és
használd a ↑/↓ billentyűket; dupla kattintásra visszaáll az alapméret); az alkalmazás
megjegyzi a magasságot. A **Minden eszköz** választóval egy eszköz (MAC-cím)
bejegyzéseire szűrhetsz. A **Szünet** megállítja a listát, hogy nyugodtan olvashasd (a
listában lefelé görgetés ugyanezt teszi); az új sorokat közben is gyűjti, számukat egy
jelvény mutatja, a **Folytatás** (vagy a lista tetejére görgetés) pedig megjeleníti őket.
Az **Exportálás…** a napló minden megőrzött sorát (a kiválasztott eszközét) szövegfájlba
menti, a legrégebbivel kezdve; a **Törlés** kiüríti a panelt. Az egymás után ismétlődő
azonos sorok egyszer jelennek meg, számlálóval (pl. ×93) és a legutóbbi időponttal; az
exportban minden sor benne van.

Az alkalmazás részletes naplófájlt is ír ide:
`~/Library/Logs/io.github.sepsig.mac-netinstall/` (`app.<dátum>.log`, az `ni-cli`
esetén `ni-cli.<dátum>.log`; naponta új fájl, az utolsó 7 marad meg). Finderben:
Ugrás → Ugrás a mappához…, és illeszd be az elérési utat.

A hibák az ablak tetején, sávban is megjelennek; a × (**Bezárás**) gombbal zárhatod be.

### Gyakori problémák

| Üzenet / tünet | Teendő |
|---|---|
| **Nincs használható vezetékes Ethernet-csatoló. …** | Csatlakoztass vezetékes Ethernet-adaptert, majd ↻. A Wi-Fi és a Thunderbolt-híd nem használható. |
| **… nincs kézzel beállított IPv4-cím a macOS-ben.** / **… nincs IPv4-címe.** | Állíts be kézi IPv4-et ([4. fejezet](#4-a-mac-előkészítése)). |
| **… DHCP-n keresztül kap IPv4-címet.** | Állítsd át kézire: Rendszerbeállítások → Hálózat → az adapter → Részletek → TCP/IP → IPv4 konfigurálása: Kézzel. |
| **nincs link – nincs bedugva a kábel?** / **Nincs bedugva a kábel, vagy ki van kapcsolva az eszköz — már most elindíthatod; a kiszolgálás a kapcsolat létrejöttekor indul.** / futás közben **Nincs kapcsolat: nincs kábel** | Dugd be a kábelt, és kapcsold be a routert. Kábel nélkül a macOS elrejti a fix címet; indítani így is lehet. |
| **Egy másik program már használja ezt a portot: 67 (BOOTP).** (vagy 69, TFTP) | Másik DHCP- vagy TFTP-szerver fut, valószínűleg az Internetmegosztás (Rendszerbeállítások → Általános → Megosztás). Kapcsold ki, és próbáld újra. |
| **…: ezen megy az alapértelmezett útvonal, tehát valódi hálózatnak tűnik.** | Figyelmeztetés: a hálózat más gépei is kaphatnak címet az alkalmazástól. Használj külön adaptert, amely csak a routerhez csatlakozik. |
| **Az alhálózatban (/…) a szerver címén kívül nincs kiosztható cím; …** | Legalább /30-as alhálózat kell. |
| **Boot kernelek:** **nem érhetők el** / **Nincsenek boot kernelek: …** | Az első letöltéshez internet kell; utána a gyorsítótárban maradnak. Futás közben a **Letöltés újra** gombbal próbálhatod újra. Offline módban ez látszik: **Offline mód: a gyorsítótárban lévő RouterOS … kerül telepítésre.** |
| **Nem sikerült meghatározni a RouterOS-verziót: …** | Ellenőrizd az internetkapcsolatot vagy a profilban megadott verziót. |
| Az eszköz nem jelenik meg a táblában | Az Etherboot mód nem sikerült (próbáld újra a reset gombbal, tovább nyomva tartva), rossz portba dugtad (ether1/BOOT), rossz a kábel, vagy nincs link. Nézd meg a naplót: ha **Nincs kiszolgálva: …** sort látsz, az ok mellette áll (pl. „nemrég telepítve (várakozási idő)”, „egy másik eszköz van kiválasztva”, „nem támogatott eszköztípus”). |
| **Az eszköz nem válaszol (lépés: …).** | A kapcsolat megszakadt telepítés közben. Ellenőrizd a kábelt, indítsd újra az eszközt Etherboot módban, és **Újra**. |
| **Az eszköz elutasította a telepítést (lépés: …).** | Az eszköz nem fogadta el a telepítést. Indítsd újra Etherboot módban, és próbáld újra; ha ismétlődik, küldj naplót. |
| **Az eszköz abbahagyta a fogadást, mielőtt minden fájlt megkapott volna (… fájl kimaradt): a telepítés nem teljes.** | Telepítsd újra. |
| **… megkapta az összes fájlt, de nem erősítette meg, hogy készen áll; a telepítést ennek ellenére lezártuk. Ellenőrizd, hogy az eszköz rendben elindul-e.** | Figyelmeztetés: a telepítés lezárult, de az eszköz nem igazolta vissza. Nézd meg, elindul-e rendben; ha nem, telepítsd újra. Ilyenkor kérlek, küldj naplót. |
| **Ez az eszköz a RouterOS … kernelével indult, ezért nem telepíthető rá ez a verzió: …. Kapcsold ki és be, hogy újra hálózatról induljon.** | Profilt (verziót) váltottál, miután az eszköz elindult. Indítsd újra Etherboot módban. |
| **A(z) … csomag „…” architektúrára készült, de ez az eszköz „…” architektúrát jelez. Semmi sem települt, és az eszköz formázása sem történt meg.** | Egy csomag nem illik az eszköz architektúrájához (minden csomag tartalmazza, milyen architektúrára készült; az alkalmazás a telepítés előtt mindet ellenőrzi). Az eszközhöz nem nyúlt. Nézd meg, hogy a megfelelő eszköz van-e csatlakoztatva; ha ismétlődik, jelezd (lásd [Visszajelzés](#12-visszajelzés)). |
| **Ennek az eszköznek az architektúrájához („…”) nem található RouterOS-csomag … Kérlek, jelezd a hibát …** | Az eszköz olyan architektúrát jelez, amelyhez az alkalmazás egyik RouterOS-letöltést sem tudta hozzárendelni. Semmi sem települt. Nyiss egy [GitHub-hibajegyet](https://github.com/SepsiG/Mac-Netinstall/issues) a teljes üzenettel (benne van a típus és az eszköz bejelentkező üzenete) és az exportált naplóval. |
| **A macOS letiltja az alkalmazás helyi hálózati hozzáférését, ezért az eszközök nem kapnak választ.** (pirossal, az állapotsorban és az eszköz sorában) | Nincs meg a Helyi hálózat engedély, vagy még nem válaszoltál a macOS kérdésére. Kattints a **Rendszerbeállítások megnyitása** gombra (Adatvédelem és biztonság → Helyi hálózat), kapcsold be a Mac Netinstallt, majd indítsd újra az eszközt Etherboot módban. Ellenőrzésképp az engedélyek ablaka is megnyílik egyszer. |
| **Sikertelen küldés (Netinstall, …×).** | A küldés más okból nem sikerült (pl. eltűnt az adapter). Ha ismétlődik, ellenőrizd a kábelt, az adaptert és az engedélyeket (**Beállítások → Engedélyek ellenőrzése**). |
| **A motor váratlanul leállt. Indítsd újra a Mac Netinstallt.** | Indítsd újra az alkalmazást, és küldd el a naplófájlt. |

## 10. Parancssor (ni-cli)

Az `ni-cli` ugyanazt a motort használja parancssorból (hibakereséshez, szkripteléshez,
hardveres teszteléshez). **Nincs benne a DMG-ben**, forrásból kell lefordítani (Rust,
rustup):

```sh
git clone https://github.com/SepsiG/Mac-Netinstall.git
cd Mac-Netinstall
cargo build --release -p ni-cli
# a bináris: target/release/ni-cli
```

Fő parancsok: `interfaces`, `preflight`, `assets`, `profiles`, `flash`. A `flash` kiírja a
felelősségkizárást, és csak akkor fut, ha a `--accept-disclaimer` kapcsolóval (vagy a
`MAC_NETINSTALL_ACCEPT_DISCLAIMER=1` környezeti változóval) elfogadod. A profilokat és a
letöltött fájlokat az alkalmazással közösen használja. Példák:

```sh
# Használható csatolók listája
ni-cli interfaces

# Ellenőrzés egy tárolt profillal
ni-cli preflight --iface en7 --profile "hAP alap"

# Egy eszköz: stabil RouterOS, meglévő konfiguráció megtartásával
ni-cli flash --accept-disclaimer --iface en7 --version stable --config keep

# Sorozat: saját szkripttel, minden új eszközre, Ctrl-C-ig;
# újraküldés nélkül (lásd Ismert korlátok)
ni-cli flash --accept-disclaimer --iface en7 --version 7.25.1 --package wifi-qcom \
  --script alap.rsc --auto-repeat --retransmits 0
```

`--auto-repeat` nélkül az első megjelenő eszközt telepíti, majd kilép. Telepítés
közben az első Ctrl-C csak figyelmeztet; 5 másodpercen belül másodszor megnyomva
megszakít. Minden opció: `ni-cli --help` és `ni-cli <parancs> --help`.

## 11. Ismert korlátok

- **Valódi hardveren csak részben kipróbálva.** Eddig egy hEX RB750Gr3-on (`mmips`)
  működött az alapértelmezett, az üres, a szkriptes és a megtartó konfigurációs mód, valamint
  az automatikus ismétlés, egy hAP ac²-n (`arm`) egy extra csomaggal, és egy
  CRS125-24G-1S-2HnD-n (`mipsbe`) is, valamint a RouterOS 6.49.23 a hEX-en. Még nem tesztelt: az arm64, smips, ppc és tile (elindul-e a boot kernelük), a módszkript, a márkajelzés eltávolítása, a licenckulcs és a telepítés utáni
  leállítás. Nem tesztelt típusoknál tartsd kéznél a hivatalos Netinstallt tartalékként.
  A nyitott pontok listája:
  [roadmap](superpowers/plans/2026-11-06-00-roadmap.md).
- Az alkalmazás alapból újraküldi a meg nem válaszolt Netinstall-kereteket; hogy a
  valódi eszközök ezt jól kezelik-e, még nem ellenőrzött. Ha egy telepítés egy lépésnél
  megakad, az `ni-cli` `--retransmits 0` kapcsolójával kikapcsolható.
- Csak Apple Silicon Mac, macOS 13 vagy újabb.
- x86 (PXE) és ARM64 UEFI eszközök nem támogatottak. RouterOS 7 és 6.30-tól RouterOS 6
  telepíthető; a v6 telepítése a RouterOS 7 netinstall kernelével működik (6.49.23 egy
  hEX-en), de a legrégebbi PowerPC-s eszközök és az arm64 v6-tal még nem kipróbáltak.
- Ad-hoc aláírás, nincs notarizálva: az első indítást engedélyezni kell (lásd
  [3. fejezet](#3-telepítés)).
- Helyi hálózat engedély nélkül az alkalmazás nem éri el az eszközöket.
- Az alkalmazás nem állítja be a Mac hálózatát; a kézi IPv4-et neked kell megadnod.

## 12. Visszajelzés

Hibát vagy javaslatot itt jelezz:
[GitHub Issues](https://github.com/SepsiG/Mac-Netinstall/issues).

Csatold:

- az exportált naplót (**Napló** → **Exportálás…**) és/vagy a naplófájlt a
  `~/Library/Logs/io.github.sepsig.mac-netinstall/` mappából;
- az eszköz típusát és architektúráját (a tábla **Típus** és **Arch.** oszlopa);
- a telepített RouterOS-verziót, és ha tudod, az eszközön előtte futót;
- a profil beállításait (verzió, csomagok, konfigurációs mód, telepítés után) –
  **licenckulcsot ne küldj**;
- a Mac típusát, a macOS-verziót és az Ethernet-adapter típusát.

## 13. Felelősség kizárása

- A Mac Netinstall szabad szoftver, amelyet **„ahogy van” alapon, mindenféle garancia
  nélkül** biztosítunk, a [GNU General Public License v3](../LICENSE) 15–16. pontja
  szerint.
- A Netinstall **törli az eszköz tárolóját**, és újratelepíti a RouterOS-t. **Saját
  felelősségedre használd.**
- A szerzők és közreműködők **semmilyen kárért vagy veszteségért nem vállalnak
  felelősséget**, beleértve azt is, ha az eszköz használhatatlanná válik („téglásodás”),
  elvész a konfiguráció vagy adat, leáll a szolgáltatás, vagy kár keletkezik régi, archív
  vagy már nem támogatott RouterOS-verzió, az eszköz minimális verziójánál régebbi verzió
  vagy rossz csomag telepítéséből.
- Éles használat előtt próbáld ki olyan eszközön, amelyet helyre tudsz állítani (pl. a
  MikroTik hivatalos Netinstalljával).
- A Mac Netinstall a MikroTikhez nem kapcsolódik, a MikroTik nem támogatja és nem hagyta
  jóvá. A MikroTik, a RouterOS és a RouterBOARD a tulajdonosuk védjegyei.

Az alkalmazás az első **Indítás** előtt egyszer kéri ennek elfogadását (és újra, ha a
szöveg megváltozik).
