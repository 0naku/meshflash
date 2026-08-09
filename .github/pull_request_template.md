## What this change does

<!-- One or two sentences. Write "Fixes #123" when an issue exists. -->

## Checks

<!-- Run these before you ask for a review. CI runs the same three. -->

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo test`

## Hardware test

A change to `src/esp.rs`, `src/dfu.rs`, `src/worker.rs`, or `src/ports.rs` can
stop a device from starting. CI cannot test those files. Select one line:

- [ ] This change does not touch the flashing path.
- [ ] This change touches the flashing path, and I tested it on hardware.
- [ ] This change touches the flashing path, but I have no device for it. This
      pull request stays open until a person with the device tests it. A
      maintainer cannot do this test for you.

For a hardware test, give these three facts:

- **Device:**
- **Firmware function and version:**
- **Result — did the device start?:**
