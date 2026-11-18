# Ethos BT Joystick

*English · [Norsk](README.no.md)*

Use a FrSky Ethos radio as a **wireless joystick** for simulators on Windows – over
Bluetooth, without a cable.

The radio sends its control channels through its Bluetooth trainer function. The app
receives the channels and presents them to Windows as an ordinary joystick
(“Ethos BT Joystick”) with 8 axes and up to 32 buttons.

Everything is in **a single exe file**. Neither vJoy nor any other third-party driver
is used: the virtual joystick is a small driver of its own that is embedded in the exe
and installed with one click.

![Screenshot](docs/screenshot.png)

## Getting started

1. **On the radio:** `Model > Trainer`, add a Bluetooth link, enable it and select
   **Slave** (CH1–CH8 or CH1–CH16).
2. Start `dist\EthosBtJoystick.exe` (see [Building](#building)).
3. Click **Install driver …** (first time only) and approve the UAC prompt.
4. Click **Search for radio**, select the radio in the list and click **Connect**.
   Pairing in Windows is not needed.
5. Select **Ethos BT Joystick** as the controller in the simulator and calibrate it
   there.

The app remembers the radio and connects automatically the next time. If the
connection drops, it reconnects by itself.

### Channels, axes and buttons

- **Axes:** each of the 8 axes (X, Y, Z, Rx, Ry, Rz, Slider, Dial) can take its value
  from any channel and can be inverted. 988–2012 µs from the radio gives full
  deflection.
- **Buttons:** a switch channel can be turned into joystick buttons. A button is
  pressed for as long as the channel is in the selected position: low (≤ 1300 µs),
  middle or high (≥ 1700 µs). A three-position switch therefore becomes three buttons
  on the same channel.
- **Signal loss:** after 0.5 s without data the axes are centred and the buttons
  released (can be changed to “keep the last values”).

The **Test in Windows (joy.cpl)** button opens Windows' own game controller test.

Settings are stored in `%APPDATA%\EthosBtJoystick\config.json`.
`EthosBtJoystick.exe --demo` shows the window with simulated channels, without a radio.

## What does the driver installation do?

A virtual joystick always requires a driver on Windows. This one is a
**user-mode driver (UMDF 2)** – it runs as an ordinary process (`WUDFHost.exe`), not
in the Windows kernel.

Windows only installs signed driver packages. When you click “Install driver”, this
happens (with administrator rights):

1. The driver package is unpacked to `C:\Program Files\EthosBtJoystick\driver`.
2. A self-signed certificate is created **on your PC**, the driver catalog is signed
   with it, and its public part is added as trusted (the machine's *Trusted Root* and
   *Trusted Publishers* stores).
3. **The private key is deleted immediately.** The certificate can therefore never be
   used to sign anything else – neither by you nor by anyone else.
4. The device `root\EthosBtJoystick` is created and the driver is installed on it.

**Uninstall** removes the device, the driver package and the certificate again.
The log of the last installation is in `C:\Program Files\EthosBtJoystick\install.log`
and is also shown in the app.

## How it works

```
 Ethos radio                       PC
┌───────────┐  Bluetooth LE   ┌──────────────────────────────────────────────┐
│ Trainer   │  service FFF0   │ EthosBtJoystick.exe                          │
│ (Slave)   │ ──────────────► │  ble.rs       receives notifications on FFF6 │
└───────────┘  charact. FFF6  │  protocol.rs  decodes the trainer frames     │
                              │  config.rs    channel → axis/button          │
                              │  joystick.rs  writes HID report 2 ───────┐   │
                              └──────────────────────────────────────────┼───┘
                              ┌──────────────────────────────────────────▼───┐
                              │ ethosjoy.dll (UMDF 2 HID minidriver)         │
                              │  delivers the data as joystick report 1      │
                              └──────────────────────────────────────────┬───┘
                                                    Windows HID / DirectInput
                                                                         ▼
                                                                    Simulator
```

**Protocol.** The radio sends frames as `0x7E <body> 0x7E`, where the body is `0x80`,
three bytes per channel pair (two 12-bit values in µs) and an XOR checksum. `0x7D` is
the escape byte (next byte XOR `0x20`). 8 channels give 14 bytes, 16 channels 26 bytes.
This is the same format as in OpenTX/EdgeTX (`bluetooth.cpp`).

**Driver.** The driver has two HID collections: the joystick itself (what the
simulator sees) and a vendor-defined control collection that the app writes to. It
contains no logic beyond passing the values on.

## Building

Requirements: Rust (stable, `x86_64-pc-windows-msvc`) and Visual Studio 2022 Build
Tools with the C++ tools and a Windows SDK.

```powershell
# once: fetch Microsoft's WDK (about 111 MB) into tools\wdk-nuget – installs nothing
powershell -ExecutionPolicy Bypass -File tools\get-wdk.ps1

# build driver + app (add -Test to run the unit tests)
powershell -ExecutionPolicy Bypass -File build.ps1
```

The result ends up in `dist\EthosBtJoystick.exe`.

| Folder | Contents |
| --- | --- |
| `app\` | The app (Rust, egui). `cargo test` runs the unit tests. |
| `driver\` | The driver (C), INF file and build script. |
| `tools\` | Script that fetches the WDK. |
| `app\examples\` | Developer tools: `scan` (Bluetooth search), `hidtree` (HID lookup) and `joycheck` (writes to the installed driver and reads back). |

## Troubleshooting

- **The radio does not show up in the list:** check that the Bluetooth trainer is set
  to *Slave* and enabled, and that Bluetooth is on in Windows. Radios are marked
  “FrSky” in the list. If needed, tick “Show unnamed devices”.
- **“Connected, waiting for data”:** the radio is connected but is not sending trainer
  frames. Check the trainer setup of the model.
- **The driver “did not start (code …)”:** open the installation log in the app. Try
  “Reinstall”. The code is the same one Device Manager shows.
- **The simulator does not find the joystick:** check with “Test in Windows (joy.cpl)”
  first. Some simulators have to be restarted after the driver has been installed.

## Limitations

- The driver requires **Windows 11** (64-bit). The project has only been built and
  tried on Windows 11.
- Smart App Control or company policies that block unsigned programs can stop both
  the app and the driver.

## Thanks to

- [FrSky-Bluetooth-Trainer](https://github.com/garfield2412/FrSky-Bluetooth-Trainer)
  for inspiration and for confirming the frame format used by Ethos.
- OpenTX/EdgeTX for the original Bluetooth trainer protocol.
- Microsoft's driver sample `vhidmini2`, from which the request handling in the driver
  is derived.

## License

- The project is licensed under the **MIT License**, see [LICENSE](LICENSE).
- Exception: the contents of the `driver/` folder are derived from Microsoft's
  `vhidmini2` sample and are licensed under the **Microsoft Public License (MS-PL)**,
  see [driver/LICENSE](driver/LICENSE).

This is an independent hobby project and is not affiliated with FrSky. FrSky and ETHOS
are trademarks of their respective owners.
