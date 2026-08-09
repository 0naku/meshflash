# Contributing to MeshFlash

Thank you for your interest in MeshFlash. This page tells you how to report a
problem and how to submit a change.

MeshFlash writes firmware to real radios. A bad change can stop a device from
starting. For this reason, this page asks for more hardware information than a
usual project.

## Report a problem

Open an issue on GitHub. Include this information:

- The name of the device, as it is shown in the MeshFlash catalog.
- The chip family: ESP32 or nRF52.
- Your operating system and its version.
- The MeshFlash version, or the commit hash if you built from source.
- The full text of the error, copied from the log panel.
- The steps that cause the problem.

If MeshFlash selected the wrong serial port, add the USB vendor ID and product
ID. The port list shows both values.

## Before you write code

Open an issue first for a large change, such as a new chip family or a new tab.
Then we can agree on the approach before you spend time on it. Small
corrections need no issue.

## Set up the build

You cannot push to the main repository. Work in a fork of it.

1. Install Rust 1.95 or newer with [rustup](https://rustup.rs). A version from
   your operating system is usually too old.
2. Click **Fork** on the GitHub page of the project.
3. Clone your fork, and use your own account name:

```
git clone https://github.com/YOUR-NAME/meshflash.git
cd meshflash
```

4. On Linux, install the development packages for `libudev` and GTK. On Debian
   and Ubuntu, run this command:

```
sudo apt install libudev-dev libgtk-3-dev
```

5. Run `cargo build` in the project folder.

## Send your change

1. Make a branch for your work:

```
git switch -c short-name-for-the-change
```

2. Write the change. Then run the three checks in the next section.
3. Commit and push to your fork:

```
git push -u origin short-name-for-the-change
```

4. Open a pull request against the `main` branch of the main repository. GitHub
   shows a button for this after your push.
5. Fill in the pull request form. It asks which checks you ran, and it asks for
   the result of a hardware test.

CI runs on your pull request. It does the same three checks, and it builds on
Windows, Linux, and macOS.

## Run the checks

Run these three commands before you open a pull request. The CI job runs the
same three commands.

```
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## Test on hardware

CI cannot flash a radio. Only a person with the device can prove that a change
works.

CAUTION: Test a flashing change only on a device that you can recover. A failed
flash stops the device until you recover it. The recovery steps are different
for each device. Read question 6.7 in the
[MeshCore FAQ](https://github.com/meshcore-dev/MeshCore/blob/main/docs/faq.md)
before you start.

If your change touches the flashing path, do a hardware test. Then write these
facts in the pull request:

- The device that you used.
- The firmware function and the version that you flashed.
- The result: the device started, or it did not start.

An untested flashing change is not merged. The person who sends the change does
the test and gives the proof.

When you cannot do the test, say so in the pull request. It then stays open
until a person with that device tests it and writes the result there.

The nRF52 path needs help most, for the same reason. The protocol code passes
unit tests, but the full DFU flow has no hardware test yet. See the Status
section of the [README](README.md).

## Code style

- Keep the standard format from `cargo fmt`. Do not change the format settings.
- Keep each module for one subject. The module comment at the top of each file
  gives its subject.
- Write comments for the reason, not for the action. The code shows the action.
- Do not add a dependency for a small function. Each new dependency makes the
  binary larger and adds risk.

## Application icon

The icon is a white lightning bolt on a blue badge. It uses the two brand
colors from `src/app.rs`: `ACCENT` and `ACCENT_DIM`.

| File | Purpose |
|---|---|
| `assets/icon-64.rgba` | The window icon. Raw RGBA pixels, 64x64. |
| `assets/icon.ico` | The Windows `.exe` icon. Seven sizes, 16 to 256. |
| `docs/images/icon.png` | A preview, for people who read the repository. |

`assets/icon-64.rgba` holds raw pixels, not a PNG. The program can then set the
window icon without an image decoder in the binary.

To change the icon, replace all three files. Keep these two formats:

- `assets/icon-64.rgba`: straight RGBA, 64x64, in row order. The file must be
  exactly 16384 bytes. The program does not decode it.
- `assets/icon.ico`: an icon file with sizes 16, 24, 32, 48, 64, 128, and 256.

Open an issue before you change the artwork. Then we can agree on the design
first.

## Make a release

CI builds the release. Do not upload a binary by hand.

1. Set the new version in `Cargo.toml`.
2. Run `cargo build` so that `Cargo.lock` gets the new version.
3. Commit both files.
4. Make the tag and push it:

```
git tag v0.2.0
git push origin v0.2.0
```

The tag must agree with the version in `Cargo.toml`. The workflow stops with an
error when the two are different.

CI then builds Windows, Linux, and macOS, and it makes a **draft** release with
the three archives and a SHA-256 file for each one. Read the draft, then publish
it by hand. A draft gives you the last check before users get a flasher that can
stop a device.

Each archive holds the program, the README, `LICENSE`, and
`THIRD_PARTY_NOTICES.md`. The two licenses ask for their notice in each
distribution, so do not publish a binary without those files.

### When the workflow fails

The release step needs all three builds. If one platform fails, you get no
release at all. You do not get a release with two archives.

For a fault in the code, such as a compile error on macOS:

1. Correct the code on `main`.
2. Delete the tag on GitHub and on your computer:

```
git push --delete origin v0.2.0
git tag -d v0.2.0
```

3. When the workflow already made a draft release, delete that draft. A second
   run cannot make a release that is there.
4. Make the tag again and push it.

For a fault outside the code, such as a network error, open the run page on
GitHub and use **Re-run all jobs**. The tag then stays as it is.

## Keep the catalog in agreement with the web flasher

The files `assets/config.json` and `assets/releases.json` are copies from
[flasher.meshcore.io](https://github.com/meshcore-dev/flasher.meshcore.io). They
are the offline fallback data.

Do not edit these two files by hand. To add a device, send the change to the web
flasher project first. Then copy the new file into MeshFlash.

## License of your contribution

MeshFlash uses the MIT License. When you submit a pull request, you agree that
your work is released under that license. See [LICENSE](LICENSE).

If you port code from another project, add it to
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). Name the file that you ported,
the source project, and its license.
