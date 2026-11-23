# Output audio as a dictation source (macOS Intel / Apple Silicon)

**Status:** implementation in progress on branch `feature/macos-output-as-dictation-source`; this document tracks the remaining work. **Not user-ready.**

## Problem

Hex can dictate from selected microphone inputs and can capture system playback separately for Meetings. It cannot yet let a user select an **output device whose playback becomes dictation input**. A user on an Intel MacBook Pro 2019 (macOS 15.1) needs to transcribe the sound being sent to speakers/headphones without sending it through a physical microphone.

## Relevant existing code

- `src/audio.rs` — `AudioInput`, recovering input selection, cpal input picker, PCM timestamps and route-change listeners
- `src/rune_ui/general.rs` and `src/rune_ui/settings.rs` — General › Microphone selector
- `src/app_settings.rs` — JSON configuration + live microphone-selection revision
- `src/meeting/macos.rs` — existing Core Audio process tap + ScreenCaptureKit fallback for system audio
- `src/meeting/capture.rs` — recoverable, supervised 16 kHz PCM sources
- `src/audio/hex_microphone.rs` — virtual **output** that should never be captured back into itself
- `docs/intel-macos.md`, draft PR #1 — existing Intel CPU-first build; do not fork the audio algorithm just for Intel

## Approach

The new native capture path targets **macOS 14.2+**, not a CPU instruction set. Apple Silicon and Intel use the **same Core Audio tap API**; the distribution package and speech backend differ.

### Stage 1 — native output tap primitive (started)

The existing meeting tap supports the all-output process mix. Introduce `ProcessTapSource::start_for_output(Option<&str>)`, where a `Some(device_uid)` selects **stream 0** of that device with
`CATapDescription::initExcludingProcesses_andDeviceUID_withStream`, while `None` retains the existing whole-system tap. Hex's process is excluded to prevent self-feedback. Keep the Meetings call site unchanged.

**Limitation:** the native backend currently has no UI/setting or connection to dictation. A device with multiple hardware output streams needs explicit treatment in a later iteration; output-device selection is not automatically the same as choosing left/right audio channel.

### Stage 2 — bring playback into the dictation PCM pipeline

- Extend source selection to tagged options: `Mic(System default or UID)` and `SystemOutput(System default or UID, stream index=0)`. Do **not** overload microphone names with magic prefix strings.
- Enumerate Core Audio output devices, persist the UID and show a friendly name. `System default output` tracks route-change notifications.
- Reuse the existing tap's mono decode and resampler rather than inventing a new driver; channel-to-mono downmix must be tested.
- Deliver timestamped PCM into the existing `DictationCapture` / recognition path with bounded buffers, idle-only changes, orderly shutdown and restart on loss.
- If the user-selected output disappears, show a **disconnected** state and recover when it returns. Never silently revert to the physical microphone or capture an unrequested output.

### Stage 3 — Labs UX, safety and consent

- Add a **Labs** item, macOS only and **off by default**, as required by `AGENTS.md`. Show `Audio source: Microphone | System output`; selecting output reveals the output-device picker.
- Ask for **Screen & System Audio Recording** access. Handle denied/not determined with actionable UI. ScreenCaptureKit currently captures the **whole system**, so do not silently substitute it for a selected device.
- **Critical:** while transcribing system playback, don't apply the normal “While dictating: Mute/Duck/Pause media” policy to that very output. Otherwise the stream records silence. Avoid capturing Hex's own feedback sounds; preserve the policy for normal microphone dictation.
- A recording (including its release tail) must finish before changing source. Show clear live source state and capture level.

### Stage 4 — compatibility and verification

- Intel Mac: test the feature inside the existing `Hex Test.app` workflow on `macos-15-intel`. Keep speech inference on CPU by default as explained in `docs/intel-macos.md`; Radeon Metal inference has known correctness problems unrelated to Core Audio.
- Apple Silicon: verify unchanged legacy mic and Meetings behavior; test targeted tap on a separate device.
- Tests: source/UID selection and migration, current/default route changes, multi-channel downmix, permission denial, unplug/reconnect, device swaps during capture, silent output, own-process exclusion and no mute/duck.
- Update Labs registry, `site/src/labs.json`, feature map, and Rune window goldens when the UI is implemented, per `AGENTS.md`.
- No merge until a macOS compile + a real playback-to-text test. GitHub's Linux sandbox cannot validate hardware audio.

## Interim tested-by-design route (not yet verified on this Mac)

Until the native feature lands, macOS Audio MIDI Setup + [BlackHole 2ch](https://existential.audio/blackhole/) provides a virtual input (Intel and Apple Silicon): create a Multi-Output Device including the speaker/headphone device and BlackHole; select that Multi-Output Device as macOS output and `BlackHole 2ch` as Hex's existing Microphone. It does not implement per-device native tapping.

## References

- [Apple: Capturing system audio with Core Audio taps](https://developer.apple.com/documentation/coreaudio/capturing-system-audio-with-core-audio-taps)
- [Apple: CATapDescription initializer scoped to an output device](https://developer.apple.com/documentation/coreaudio/catapdescription/initexcludingprocesses:anddeviceuid:withstream:)
- [Intel fork PR #1](https://github.com/jirbis/hex/pull/1)
