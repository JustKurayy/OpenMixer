# OpenMixer

A small native desktop mixer for listening to a line input without replacing or rerouting the rest of your system audio. OpenMixer was built for an Audio-Technica AT-LP60XBT connected to a PC: select one capture device, clean up steady electrical noise, shape the tone, and listen through the operating system's normal output.

![OpenMixer mixer interface](docs/openmixer.png)

## What it does

- Monitors only the input you select and follows the operating system's default playback device. It never changes that default.
- Removes 50 Hz or 60 Hz mains hum and its first harmonics.
- Starts with cleanup off for a transparent signal. Optional two-second noise learning enables stereo-linked spectral reduction.
- Adds a 28 Hz rumble filter and ±12 dB bass/treble controls.
- Provides a 0–100% master fader. Overdrive extends it to 300% while peak protection remains active.
- Shows a stereo program meter plus a calibrated A-weighted loudness estimate with green, orange, and red risk zones.
- Runs as a native Rust application on Windows, macOS, and Linux through CPAL and egui.

The large readout is an **estimated dB(A)** value. A-weighting is applied in the audio engine, but a line input cannot know the gain of your amplifier, speaker sensitivity, listening distance, or the system volume after OpenMixer. Calibrate the readout against a real sound meter at your listening position before treating it as meaningful. Green is below 85 dB(A), orange is 85–89 dB(A), and red is 90 dB(A) or higher. [NIOSH uses 85 dB(A) averaged over eight hours](https://www.cdc.gov/niosh/noise/about/noise.html) as its recommended exposure limit; loudness and exposure time both matter.

## Use it with the AT-LP60XBT

1. Put the turntable's rear pre-amplifier switch in **LINE** when connecting it to a PC line input. `PHONO` requires a separate phono preamp.
2. Connect the turntable to a real stereo line input. Many laptop microphone jacks are mono and add gain/noise; a small USB audio interface with line inputs usually works better.
3. Disable Windows/macOS/Linux "Listen to this device" or other direct monitoring for the same input. OpenMixer should be the only monitoring path.
4. Choose your speakers or headphones as the operating system's normal default output.
5. Run OpenMixer, choose the line input, start at the default 25% level, and press **LISTEN**.
6. If you hear hum, select the local mains frequency: 50 Hz in most of Europe, including the Netherlands, or 60 Hz in North America. Leave it off when it does not help.
7. For steady hiss, stop the record and press **LEARN NOISE**. Increase denoise only as far as needed; spectral reduction can soften quiet musical detail.
8. To calibrate dB(A), play steady audio and adjust the calibration slider until OpenMixer matches a sound meter placed at your listening position.

If the noise exists before the turntable starts, also check the cable, PC input gain, and audio interface. Try keeping the turntable and PC/interface on the same power strip, and test a different line input or shielded cable. Software can reduce steady hum and hiss, but it cannot repair clipping or remove the electrical cause of a ground loop.

## Run from source

Install the current stable [Rust toolchain](https://rustup.rs/), then:

```console
cargo run --release
```

The optimized Windows binary is created at `target/release/openmixer.exe`.

### Windows

Install Rust with the MSVC toolchain and the Visual Studio C++ Build Tools. OpenMixer uses the normal Windows audio backend and does not install a driver.

### macOS

Install Xcode Command Line Tools and Rust. macOS may request microphone permission because line inputs are classified as capture devices.

### Linux

Install Rust plus the native audio/window development packages. On Debian or Ubuntu:

```console
sudo apt install build-essential pkg-config libasound2-dev libx11-dev libxi-dev libxcursor-dev libxrandr-dev libxkbcommon-dev libwayland-dev
cargo run --release
```

## Signal path

```text
selected line input
  → 28 Hz low cut
  → 50/60 Hz harmonic notches
  → learned spectral reduction
  → bass and treble
  → smoothed master gain / mute
  → peak protection
  → operating system default output
```

OpenMixer currently requires the selected devices to share a 48 kHz or 44.1 kHz mode and supports `f32`, `i16`, and `u16` sample formats. It uses about 50 ms of safety buffering plus the spectral window; actual end-to-end latency depends on the hardware and operating system and has not been measured here. Device disconnection, buffer underruns, and overflows are shown in the interface.

The Windows build and UI were verified on the development machine. macOS and Linux use the same cross-platform code but still need hardware testing. Crackle restoration, recording, plugins, multiple buses, virtual audio devices, installers, and automatic updates are intentionally outside this small first release.

On the Windows development machine, the stripped release executable measured 8.0 MiB. With the interface open and audio stopped, one three-second sample measured 74.1 MiB working set and 2.6% of one CPU core. These are a single-machine idle snapshot, not cross-platform guarantees; active DSP usage still needs measurement with the actual line input.

## Development

```console
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

Licensed under the [GNU General Public License v3](LICENSE).
