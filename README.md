# Conductor

Control your Windows PCs from your other devices, over your own [Tailscale](https://tailscale.com) network. There are no accounts and no servers in the middle: every PC you install Conductor on can be seen and controlled by your other devices on the same tailnet, and by nobody else.

- Full keyboard and mouse control, on every display
- Works at the sign-in screen, the lock screen, and on UAC prompts
- Hardware video encoding with low latency, and adapts to the connection
- Sound from the PC you're controlling
- Copy and paste text between PCs, and send or save files
- Shows who's in control on the PC's screen, or covers the screen so nobody at the PC sees what you're doing
- Sends nothing while nobody is watching, and nothing for sound while it's quiet
- Sleep, restart or shut down a PC from your other devices, and wake it again from another PC on its network once a test wake has worked
- Updates itself from GitHub releases when nobody is using it
- Control from the Windows app or any browser on your tailnet, with touch controls on phones and tablets

Conductor is under active development. Some features listed in the [roadmap](#roadmap) aren't built yet.

## Requirements

- Windows 10 or 11 on each PC you want to control
- Tailscale on every device, signed in to the same account
- In Tailscale on each PC, turn on **Run unattended** (right-click the Tailscale icon, then Preferences). Without it, Tailscale disconnects when you sign out, and the PC can't be reached until someone signs in again.

## Install

Download the latest installer from [Releases](https://github.com/hythe55/Conductor/releases) and run it on each PC. The installer sets up a small background service so the PC stays reachable after a restart. You can turn this off in Conductor's settings.

Windows may warn that the installer is from an unknown publisher, because it isn't code-signed yet. Choose **More info**, then **Run anyway**.

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

The Android app also needs JDK 17 and the Android SDK and NDK, with `JAVA_HOME`, `ANDROID_HOME` and `NDK_HOME` set. Build it with `npx tauri android build --apk` in `app`. On Windows, turn on Developer Mode first: the build creates symbolic links. The app asks for the name of one of your PCs the first time it opens, since a phone can't list the devices on your tailnet itself.

## Roadmap

- Android app downloads (it builds from source today)
- Virtual displays, sized to the device you're controlling from
- Sound even when the PC's own speakers are muted
- Scheduled sleep, shutdown and restart

## License

[GPL-3.0](LICENSE)
