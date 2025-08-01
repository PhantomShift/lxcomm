# LXCOMM - Linux (XCOM2) Mod Manager

<div align="center">

![icon](assets/lxcomm.svg)

</div>

Mod browser, downloader and manager made specifically for non-Steam versions of XCOM2 (War of the Chosen) on Linux.

## Caveats

Before getting into more of the project,
I want to get out of the way all the **caveats** that I believe
users should be aware of coming into using LXCOMM:

1) **If you're on Windows, use [Alternative Mod Launcher](https://github.com/X2CommunityCore/xcom2-launcher) (AML).** Although I have actually written the project to be usable on both Linux and Windows where possible, Linux is very much the focus of this project with Windows seeing minimum viable support.
Additionally, the project makes extensive use of symlinks which means you must run LXCOMM as administrator, run Windows in developer mode or go through the annoying process of enabling symlinks (Windows non-Home only, unless you're a wizard or something).
2) **If you're not on Windows, use AML.** AML is a much more mature project with a large backing by the community. Although the inconvenience of setting it up on Linux
for a non-Steam installation was why I started writing this project in the first place,
it is still possible to set up and is still the ***best*** solution for mod management in XCOM2 (and is [working on proton again](https://www.reddit.com/r/xcom2mods/comments/1mcsfe1/proton_users_1002beta_is_aml_friendly_again1/) it seems!).
3) **Steam login and Steam Web API Key required.** As much as possible, I would like to refrain
from resorting to scraping Steam's resources or using non-official tools for downloading Workshop items in LXCOMM.
As such, a Steam account *is* required for downloading mods through LXCOMM via [SteamCMD](https://developer.valvesoftware.com/wiki/SteamCMD)
(although you do *not* need to own XCOM2 *on Steam* to download mods, thankfully, you just need to not be anonymous),
and for browsing and automatically retrieving workshop information, a Steam web API key is needed.
(Saving these account details is an opt-in action via the settings.)
4) As of writing, **this is alpha software**. For one, I'm actually new to modding XCOM 2 after
finally getting around to beating the game and finding myself seeking more.
Although the project as it stands is in a usable-enough state for modding your game,
there are a lot of quality of life features that I still want to add (you can take a look at [Tracking Ideas](#tracking-ideas)) and there are likely lots of bugs that I've yet to uncover and fix.
You use LXCOMM at your own risk (though bug reports are appreciated!)

If you read all that and you've still decided that you want to at least try out LXCOMM,
then feel free to install and read the [startup guide](https://github.com/PhantomShift/lxcomm/wiki/Startup-Guide).

## Installation

### Releases

Binaries for versioned releases of supported platforms are available to download at [Releases](https://github.com/PhantomShift/lxcomm/releases/latest).

### Package Manager

As of writing, LXCOMM is only packaged in the AUR.
If anyone is interested in maintaining packages elsewhere,
please feel free to open an issue for communicating any needs or wants.

```bash
# Replace `paru` with your AUR helper of choice
# From source
paru -Syu lxcomm
# Binary-only package
paru -Syu lxcomm-bin
```

### Manual Installation

```bash
# Minimal one-step installation
cargo install --git https://github.com/phantomshift/lxcomm.git

# For Linux desktop integration (user-local)
# Ensure ~/.cargo/bin is included in your path
git clone https://github.com/phantomshift/lxcomm.git
cargo install --path lxcomm
cp lxcomm/dist/lxcomm.desktop ~/.local/share/applications/
cp lxcomm/assets/lxcomm.svg ~/.local/share/icons/
```

## Other Platforms

The main intended place to be running the program is on Linux,
but at a best effort, LXCOMM is written to be cross-platform.
However, in particular, macOS cannot be fully verified as working
by me as I do not have a machine to develop on.
Regardless, I am very much open to receiving suggestions or pull requests
for platform-specific fixes.

## Tracking Ideas

In no particular order:

- [x] Option to log out of steamcmd on exit (default command: quit)
- [x] Text editor + diffing for per-profile mod ini settings
- [x] On detailed view open, resolve unknown dependencies in the background
- [x] Filter library with option for [fuzzy finding](https://github.com/Blakeinstein/fuse-rust)
- [x] ~~(Blocked on [#1](https://github.com/PhantomShift/lxcomm/issues/1)) Check for updates without having to invoke downloads on all mods (build manifest file from scratch and then try running `steamcmd +workshop_status`?)~~ Updates are now checked via the Steam Web API.
- [x] Automatically find game installations and folders (using [walkdir](https://github.com/BurntSushi/walkdir)?)
- [x] Options for ranking search results in browsing
- [x] Per-profile character pools
- [x] Read most recent game logs
- [x] Recurse full mod dependency tree when checking for missing dependencies
- [x] Support manually downloaded mods
- [x] Support workshop collections
- [ ] Copying/importing+exporting profiles
- [ ] Disk cache trimming
- [x] ~~Ability to enter workshop ID directly for browsing or downloading (due to unreliable browsing results)~~ Search results should now be all-inclusive
- [ ] Investigate controller navigation in iced (or libcosmic) (for the Steam Deck gamers)
- [ ] Make profile page widgets resizable

## License

lxcomm is dual-licensed under [MIT](LICENSE-MIT) or [Apache License 2.0](LICENSE-APACHE) at a given contributor's choice,
falling back to Apache 2.0 unless otherwise stated.
