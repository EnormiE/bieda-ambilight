# bieda-ambilight

A lightweight Windows background utility (living in the system tray) built specifically for **Robobloq** USB LED strips (`0x1A86:0xFE07`) and compatible **DX Light** controllers - **not** a universal RGB/ARGB controller for all LED strips.

Written in Rust with no console window and minimal CPU/RAM usage, it replaces the original control software with a local, privacy-friendly tray utility.

## Features

- **Screen Sync (`Game` & `Movie`):** Real-time edge sampling with 34-range packet compression over USB HID, exponential smoothing for movies, and selectable refresh rate (`Auto`, `30 FPS`, `60 FPS`).
- **Multi-Monitor Support:** Defaults to the primary display with manual monitor selection in the tray menu and automatic handle refresh after display/resolution changes.
- **Static Color:** Instant solid color mode with a native Windows color picker dialog (`ChooseColorW`).
- **Built-in Effects (`Dynamic` & `Rhythm`):** Hardware-accelerated dynamic animations (with adjustable speed) and music-reactive modes supporting both Windows system audio peak metering and the controller's built-in microphone (with adjustable sensitivity).
- **System Tray & Persistence:** Runs quietly in the background without a console window, remembers state in `config.toml`, supports auto-off on screen lock/shutdown, prevents duplicate instances via a named mutex, and reconnects automatically after USB disconnects.

## Download & Hardware Compatibility

- **Prebuilt binary:** Download `bieda-ambilight.exe` from the [Releases](../../releases/latest) page - no Rust installation required.
- **OS:** Windows (x64)
- **Hardware:** Built and tested for the Robobloq USB LED strip (`VID: 0x1A86`, `PID: 0xFE07`, HID Usage Page `0xFF00` / Usage `0x01`). It will not work out of the box with motherboard ARGB headers, Wi-Fi/Bluetooth strips, or USB controllers using a different protocol.

## Build from Source

### Requirements (for building from source only)

- **OS:** Windows (x64)
- **Rust toolchain:** `cargo` and `rustc` (`msvc` target recommended on Windows)

### Build & Run

Clone the repository and build an optimized release binary:

```bash
cargo build --release
```

The compiled executable will be located at `target\release\bieda-ambilight.exe`.

### Autostart (Optional)

1. Press `Win + R`, type `shell:startup`, and press **Enter**.
2. Place a shortcut to `bieda-ambilight.exe` in the Startup folder.

## Configuration

On first launch, `config.toml` is created next to the executable. You can edit LED counts and default values there (program can't be running):

```toml
mode = "sync_game"
brightness = 230
sensitivity = 50
dynamic_speed = 50
sync_fps = "auto"
monitor_name = ""
reverse = true
auto_off = true
rhythm_windows_audio = true
leds_left = 12
leds_top = 33
leds_right = 12
static_color = [255, 140, 0]
```

### Adjusting LED Counts (Monitor Size)

The default values (`12 / 33 / 12`) are set for a **24"** 3-sided strip. If colors look shifted or part of the strip stays off, edit `leds_left`, `leds_top`, and `leds_right` in `config.toml` (located next to `bieda-ambilight.exe`, or in `%APPDATA%\bieda-ambilight\config.toml` if run from a read-only folder) and restart the app:

| Monitor Size (3-sided strip) | `leds_left` | `leds_top` | `leds_right` |
| --- | :---: | :---: | :---: |
| **24"** *(default)* | `12` | `33` | `12` |
| **27"** | `18` | `30` | `18` |

For other sizes or strip variants, count the LEDs along each edge and update the values accordingly.

## Known Limitations

- **Windows HDR:** Screen Sync relies on standard SDR screen capture (`xcap`). When HDR is enabled in Windows Display Settings, captured colors may appear washed out or overly bright on the LED strip.

## Acknowledgments

Protocol details and packet structure were pieced together with help from **OpenRGB** references and reverse engineering the **DX Light** control app.

## Disclaimer

This project is an independent, lightweight replacement utility and is not affiliated with or endorsed by Robobloq or DX Light.

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.
