# Ethos BT Joystick

*Norsk · [English](README.md)*

Bruk en FrSky Ethos-radio som **trådløs joystick** for simulatorer på Windows – over
Bluetooth, uten kabel.

Radioen sender styrekanalene sine via Bluetooth-trainer-funksjonen. Appen tar imot
kanalene og presenterer dem for Windows som en vanlig joystick («Ethos BT Joystick»)
med 8 akser og opptil 32 knapper.

Alt ligger i **én exe-fil**. Det brukes verken vJoy eller andre tredjepartsdrivere:
den virtuelle joysticken er en egen liten driver som følger med i exe-filen og
installeres med ett klikk.

![Skjermbilde](docs/skjermbilde.png)

## Kom i gang

1. **På radioen:** `Model > Trainer`, legg til en Bluetooth-link, slå den på og velg
   **Slave** (CH1–CH8 eller CH1–CH16).
2. Last ned zip-filen fra
   [siste release](https://github.com/bliatun-code/EthosBtJoystick/releases/latest),
   pakk den ut og start `EthosBtJoystick.exe`. Windows kan advare om ukjent utgiver
   første gang, fordi programmet ikke er kodesignert.
3. Klikk **Installer driver …** (bare første gang) og godkjenn UAC-spørsmålet.
4. Klikk **Søk etter radio**, velg radioen i listen og klikk **Koble til**.
   Windows-paring er ikke nødvendig.
5. Velg **Ethos BT Joystick** som kontroller i simulatoren og kalibrer der.

Appen husker radioen og kobler til automatisk neste gang. Faller forbindelsen ut,
kobles det til igjen av seg selv.

### Kanaler, akser og knapper

- **Akser:** hver av de 8 aksene (X, Y, Z, Rx, Ry, Rz, Slider, Dial) kan hente verdien
  fra en valgfri kanal, og kan inverteres. 988–2012 µs fra radioen blir fullt utslag.
- **Knapper:** en bryterkanal kan gjøres om til joystick-knapper. En knapp er trykket
  så lenge kanalen står i valgt posisjon: lav (≤ 1300 µs), midt eller høy (≥ 1700 µs).
  En treveisbryter blir altså tre knapper på samme kanal.
- **Tap av signal:** etter 0,5 s uten data sentreres aksene og knappene slippes
  (kan endres til «behold siste verdier»).

Knappen **Test i Windows (joy.cpl)** åpner Windows' egen spillkontroller-test.

Innstillingene lagres i `%APPDATA%\EthosBtJoystick\config.json`.
`EthosBtJoystick.exe --demo` viser vinduet med simulerte kanaler, uten radio.

## Hva gjør driverinstallasjonen?

En virtuell joystick krever alltid en driver på Windows. Denne er en
**brukermodus-driver (UMDF 2)** – den kjører som en vanlig prosess (`WUDFHost.exe`)
og ikke i Windows-kjernen.

Windows installerer bare signerte driverpakker. Når du klikker «Installer driver»
skjer derfor dette (med administratorrettigheter):

1. Driverpakken pakkes ut til `C:\Program Files\EthosBtJoystick\driver`.
2. Det lages et selvsignert sertifikat **på din PC**, driverkatalogen signeres med
   det, og den offentlige delen legges inn som klarert (lagrene *Trusted Root* og
   *Trusted Publishers* for maskinen).
3. **Den private nøkkelen slettes med en gang.** Sertifikatet kan dermed aldri brukes
   til å signere noe annet – verken av deg eller andre.
4. Enheten `root\EthosBtJoystick` opprettes og driveren installeres på den.

**Avinstaller** fjerner enheten, driverpakken og sertifikatet igjen.
Loggen fra siste installasjon ligger i `C:\Program Files\EthosBtJoystick\install.log`
og vises også i appen.

## Slik virker det

```
 Ethos-radio                       PC
┌───────────┐  Bluetooth LE   ┌──────────────────────────────────────────────┐
│ Trainer   │  tjeneste FFF0  │ EthosBtJoystick.exe                          │
│ (Slave)   │ ──────────────► │  ble.rs       tar imot varsler på FFF6       │
└───────────┘  karakt. FFF6   │  protocol.rs  dekoder trainer-rammene        │
                              │  config.rs    kanal → akse/knapp             │
                              │  joystick.rs  skriver HID-rapport 2 ─────┐   │
                              └──────────────────────────────────────────┼───┘
                              ┌──────────────────────────────────────────▼───┐
                              │ ethosjoy.dll (UMDF 2 HID-minidriver)         │
                              │  leverer dataene som joystick-rapport 1      │
                              └──────────────────────────────────────────┬───┘
                                                    Windows HID / DirectInput
                                                                         ▼
                                                                    Simulator
```

**Protokoll.** Radioen sender rammer som `0x7E <innhold> 0x7E`, der innholdet er
`0x80`, tre byte per kanalpar (to 12-bits verdier i µs) og en XOR-sjekksum. `0x7D`
er escape-tegn (neste byte XOR `0x20`). 8 kanaler gir 14 byte, 16 kanaler 26 byte.
Dette er samme format som i OpenTX/EdgeTX (`bluetooth.cpp`).

**Driver.** Driveren har to HID-samlinger: selve joysticken (det simulatoren ser) og
en leverandørdefinert kontrollsamling som appen skriver til. Den inneholder ingen
logikk utover å videreformidle verdiene.

## Bygge selv

Krav: Rust (stable, `x86_64-pc-windows-msvc`) og Visual Studio 2022 Build Tools med
C++-verktøy og Windows SDK.

```powershell
# én gang: hent Microsofts WDK (ca. 111 MB) til tools\wdk-nuget – installerer ingenting
powershell -ExecutionPolicy Bypass -File tools\get-wdk.ps1

# bygg driver + app (legg til -Test for å kjøre enhetstestene)
powershell -ExecutionPolicy Bypass -File build.ps1
```

Resultatet havner i `dist\EthosBtJoystick.exe`. `tools\package-release.ps1` pakker
den deretter i en release-zip sammen med lisensfilene.

| Mappe | Innhold |
| --- | --- |
| `app\` | Appen (Rust, egui). `cargo test` kjører enhetstestene. |
| `driver\` | Driveren (C), INF-fil og byggeskript. |
| `tools\` | Skript for å hente WDK og pakke en release. |
| `app\examples\` | Utviklerverktøy: `scan` (Bluetooth-søk), `hidtree` (HID-oppslag) og `joycheck` (skriver til den installerte driveren og leser tilbake). |

## Feilsøking

- **Radioen dukker ikke opp i listen:** sjekk at Bluetooth-trainer står på *Slave* og
  er slått på, og at Bluetooth er på i Windows. Radioer merkes med «FrSky» i listen.
  Kryss eventuelt av for «Vis enheter uten navn».
- **«Tilkoblet, venter på data»:** radioen er tilkoblet, men sender ikke
  trainer-rammer. Sjekk trainer-oppsettet på modellen.
- **Driveren «startet ikke (kode …)»:** åpne installasjonsloggen i appen. Prøv
  «Installer på nytt». Koden er den samme som Enhetsbehandling viser.
- **Simulatoren finner ikke joysticken:** sjekk først med «Test i Windows (joy.cpl)».
  Noen simulatorer må startes på nytt etter at driveren er installert.

## Begrensninger

- Driveren krever **Windows 11** (64-bit). Prosjektet er bare bygget og prøvd på Windows 11.
- Smart App Control eller bedriftspolicyer som blokkerer usignerte programmer kan
  stoppe både appen og driveren.

## Takk til

- [FrSky-Bluetooth-Trainer](https://github.com/garfield2412/FrSky-Bluetooth-Trainer)
  for inspirasjon og bekreftelse på rammeformatet fra Ethos.
- OpenTX/EdgeTX for den opprinnelige Bluetooth-trainer-protokollen.
- Microsofts driver-eksempel `vhidmini2`, som forespørselshåndteringen i driveren er
  avledet av.

## Lisens

- Prosjektet er lisensiert under **MIT-lisensen**, se [LICENSE](LICENSE).
- Unntak: innholdet i mappen `driver/` er avledet av Microsofts eksempel `vhidmini2`
  og er lisensiert under **Microsoft Public License (MS-PL)**, se
  [driver/LICENSE](driver/LICENSE).

Dette er et uavhengig hobbyprosjekt og har ingen tilknytning til FrSky. FrSky og
ETHOS er varemerker som tilhører sine respektive eiere.
