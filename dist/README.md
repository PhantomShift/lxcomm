# `dist` Folder

This folder contains reference implementations for packaging on different platforms.

## Flatpak

> [!NOTE]
> This reference implementation exists as part of research for potentially releasing
> a flatpak version. However, at this point in time I have no plans of releasing a flatpak
> version until the project reaches some sort of "1.0" release. But, if anyone is interested
> in submitting and maintaining a flatpak on flathub for LXCOMM, feel free to open an issue
> discussing your plan of action and communicate anything you want or need for the flatpak
> to function or build properly. I know some of you play XCOM2 on Steam Deck!

The flatpak uses `cargo-vendor-filterer` to build crate dependencies offline.
Note that the manifest contained here is intended to be used for building/testing
locally and is not intended to be used directly as the manifest in an online flatpak repository.
See project root `justfile` for details.

Note that the flatpak bundles steamcmd during the build process;
see [Steam's Subscriber Agreement](https://store.steampowered.com/subscriber_agreement/)
for their usage terms. The SteamCMD `Steam_Install_Agreement` was retrieved from the
[debian package](https://metadata.ftp-master.debian.org/changelogs//non-free/s/steamcmd/steamcmd_0~20180105-4_copyright).

Some caveats with the flatpak version

- Game and local directories cannot be entered manually nor found automatically
and must be picked through the file picker due to flatpak's sandboxing
  - Users could enable automatic finding by allowing the relevant file permissions
- Currently, the download directory is hardcoded to the flatpak data directory and cannot be changed
- The steamcmd command path is hardcoded to use the bundled version of steamcmd and cannot be changed
- Launching directly through LXCOMM has very limited capabilities;
the suggested method is to use `xdg-open [url]` where `url`'s protocol is handled by a program on the user's system,
e.g. `steam://rungeameid/[id]` for launching via Steam
or `heroic://launch?appName=[id]&runner=[runner]` for Heroic Games Launcher.
