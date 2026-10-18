# Conductor

Control your Windows PCs from your other devices, over your own [Tailscale](https://tailscale.com) network. There are no accounts and no servers in the middle: every PC you install Conductor on can be seen and controlled by your other devices on the same tailnet, and by nobody else.

- Full keyboard and mouse control, on every display, or on just one window
- Draw with a pen, and drawing apps on the PC get its pressure and tilt
- Works at the sign-in screen, the lock screen, and on UAC prompts
- Hardware video encoding with low latency, and adapts to the connection
- Sound from the PC you're controlling, even while its own speakers are muted
- Copy and paste text, pictures and files between PCs, and send or save files, including files shared to the Android app from other apps
- A small picture of each PC's screen in the device list, which each PC can turn off
- Shows who's in control on the PC's screen, or covers the screen and turns off its keyboard and mouse so nobody at the PC sees or gets in the way of what you're doing
- Sends nothing while nobody is watching, and nothing for sound while it's quiet
- Sleep, restart or shut down a PC from your other devices, now or on a schedule, and wake it again from another PC on its network once a test wake has worked
- Updates itself from GitHub releases when nobody is using it
- Control from the Windows app, the Android app or any browser on your tailnet, with touch controls on phones and tablets

Conductor is under active development. Some features listed in the [roadmap](#roadmap) aren't built yet.

## Requirements

- Windows 10 or 11 on each PC you want to control
- Tailscale on every device, signed in to the same account
- In Tailscale on each PC, turn on **Run unattended** (right-click the Tailscale icon, then Preferences). Without it, Tailscale disconnects when you sign out, and the PC can't be reached until someone signs in again.

## Install

Download the latest installer from [Releases](https://github.com/hythe55/Conductor/releases) and run it on each PC. The installer sets up a small background service so the PC stays reachable after a restart. You can turn this off in Conductor's settings.

Windows may warn that the installer is from an unknown publisher, because it isn't code-signed yet. Choose **More info**, then **Run anyway**.

To control your PCs from an Android phone or tablet, download `Conductor_<version>_android-arm64.apk` from the same release and open it. Android asks you to allow installs from your browser or file manager the first time. The first time the app opens, it asks for the name of one of your PCs, since a phone can't list the devices on your tailnet itself. The Android app doesn't update itself yet: install a newer APK over it. It controls PCs but can't be controlled.

## Use it from a browser

Every PC running Conductor also serves the web client on your tailnet. Open `http://<pc-name>:4280` from any device signed in to your Tailscale account.

## Security

- A PC only accepts connections from devices that belong to the same Tailscale user as the PC itself. Conductor asks the local Tailscale service who is connecting; there are no passwords to leak.
- The firewall rule the installer adds only admits Tailscale addresses.
- Requests from websites other than Conductor itself are refused, so a page you visit can't talk to your PCs through your browser.
- Video, audio and input are encrypted end to end by WebRTC, inside Tailscale's own encryption.

## How it works

Each PC runs two pieces:

- `conductor-host service` runs as a Windows service. It keeps the agent running in whichever session is on the screen and answers the local app.
- `conductor-host agent` captures the screen with the Desktop Duplication API, encodes it with the GPU's H.264 encoder, and streams it over WebRTC, with the sound playing on the PC encoded as Opus. It only does this while someone is connected, and only sends frames when the screen changes and sound while something plays.

The Windows and Android apps are built with [Tauri](https://tauri.app). Their interface is the same web client every PC serves.

## Build from source

You need Rust (stable), Node.js 20 or newer, the Visual Studio C++ build tools, and CMake to build Opus. The build tools come with a CMake: if `cmake` isn't on your PATH, set `CMAKE` to `C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe`.

```
cd app
npm install
npm run tauri build
```

The installer ends up in `target/release/bundle/nsis/`. To run the host without installing the service, build it with `cargo build -p conductor-host` and run `target/debug/conductor-host.exe` from the repository root.

The Android app also needs JDK 17 and the Android SDK and NDK, with `JAVA_HOME`, `ANDROID_HOME` and `NDK_HOME` set. Build it with `npx tauri android build --apk` in `app`. On Windows, turn on Developer Mode first: the build creates symbolic links.

## Roadmap

- Virtual displays, sized to the device you're controlling from

## License

[GPL-3.0](LICENSE)
