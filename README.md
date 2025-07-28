# LXCOMM - Linux (XCOM2) Mod Manager

<div align="center">

![icon](assets/lxcomm_icon.svg)

</div>

Mod browser, downloader and manager made specifically for non-Steam versions of XCOM2(WOTC) on Linux.

## Startup Guide

### Preface

Before getting into the guide proper, I want to get out of the way all the **caveats** that I believe
users should be aware of coming into using LXCOMM:

1) **If you're on Windows, use [Alternative Mod Launcher](https://github.com/X2CommunityCore/xcom2-launcher) (AML).** Although I have actually written the project to be usable on both Linux and Windows where possible, Linux is very much the focus of this project with Windows seeing minimal viable support.
Additionally, the project makes extensive use of symlinks which means you must run LXCOMM as administrator, run Windows in developer mode or go through the annoying process of enabling symlinks (Windows non-Home only, unless you're a wizard or something).
2) **If you're not on Windows, use AML.** AML is a much more mature project with a large backing by the community. Although the inconvenience of setting it up on Linux
for a non-Steam installation was why I started writing this project in the first place,
it is still possible to set up and is still the ***best*** solution for mod management in XCOM2.
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

If you read all that and you've still decided that you want to at least try out LXCOMM, then read on ahead.

### Example Setup - GOG through Heroic Games Launcher

I own XCOM2 on GOG, and as such, use the Heroic Games Launcher for running the game.
Anyone who has tried running the game on Linux will know that the vanilla version is
just plain broken (you literally can't even finish the tutorial mission),
so this guide assumes that you've already set up the launcher to launch the
WOTC binary and not the vanilla version of the game.

On a fresh install, LXCOMM will prompt you for your [Steam Web API Key](https://steamcommunity.com/dev/apikey). This was mentioned in the preface, but LXCOMM uses
Steam's web API for browsing and retrieving mod information.
LXCOMM will prompt you for this key every time you open the program unless you enable the "Save API Key" setting.

Along with the web API key, LXCOMM uses SteamCMD for downloading workshop items, and as such,
requires that you login with your username, password and a Steam Guard code if you have two-factor enabled.
Ensure that you have [SteamCMD installed](https://developer.valvesoftware.com/wiki/SteamCMD#Downloading_SteamCMD).
If it cannot be found in the path, try setting it manually in the settings.
LXCOMM attempts to use a cached login when it starts up every time,
but if it fails, requires that you log in again.

To reiterate this point, if you are not comfortable with an entity other than Valve handling your Steam login credentials (as is very much reasonable), turn back now and get AML set up.

<details>
<summary>I'm getting a weird error on startup!</summary>

If you have set your credentials to be saved via the settings but LXCOMM is displaying errors
when attempting to load your saved credentials, you likely do not have a secrets provider enabled,
for example as of writing `cosmic-desktop` does not ship its own secrets provider.
For many systems, this is provided by `gnome-keyring`, but otherwise check that you have
a secrets provider for your desktop environment (i.e. kdewallet on plasma) installed and running.

</details>

Now that you're logged into SteamCMD and have an API key entered,
you can get to actually downloading and installing mods.
If you head to the `Workshop Items` tab at the top, you'll be greeted with a browser that allows
you to explore XCOM2's Steam workshop items straight from LXCOMM. It goes without saying that
the first mods you'll want to download are WOTC Community Highlander along with the Alien Hunters Community Highlander plugin if you have the Alien Hunters DLC.
Make sure that you choose *either* the beta OR the stable version
(though you can download both if you want, just make sure you add the correct ones to your profile).

From the browsing page, you can either download the mod directly, or take a look at its details
with the `View` button. For now, you can just hit download on the relevant mods you need downloaded.
Ongoing downloads can be tracked at the `Downloads` tab at the top, which shows
ongoing, queued, completed and errored downloads, in that order.
If you find that downloads fail frequently (this is largely an issue with *large* mods),
you may want to change the `Automatic Download Retries` setting.

Once a download has completed, you'll see your new mod populate your library in the `Library` page.
This page contains *all* mods that you've downloaded through LXCOMM (support for local mods pending).
Assuming you've downloaded Community Highlander and the Alien Hunters plugin,
you're ready to create a new *profile*.

In LXCOMM, a *profile* is essentially a build configuration for a modded XCOM 2 session.
As of writing, profiles contain a list of mods that will be active, any modified INI files,
and its own separate save directory to ensure that different configurations do not taint/corrupt each other's save files.
If you'd like to reuse a save, you can import a save from a directory, although this will overwrite any existing saves in the profile.

To create a profile, head to the `Profiles` page and click `Add Profile +`.
You'll be prompted for a unique name, and a fresh profile will be created.
Next, head back to the library page and select your mods using the checkboxes.
Click `Add to Profile` at the top, select the profile you just created, and hit `Confirm`.
If you head back to the `Profiles` page and select the profile you created,
you'll now find that the mods list is now populated. This is the list of mods that will be active when the configuration is built.
This list, at best effort, will indicate that there is an issue with mod compatibility (missing dependencies, incompatible mods, missing information) by highlighting the entry **red**. Community Highlander doesn't depend on any other mods, so everything should be all clear.

At last, we'll be heading over to the `Main` page, which contains the upfront configuration for your modded session. The options you'll find are

- **Active Profile** - The *profile* that will be used for your session. Set this to the profile you just created.
- **Game Directory** - The directory where the game's own files are found. For a WOTC setup, this should be the installed game folder that's called `XCOM2-WarOfTheChosen`.
- **Local Directory** - The directory that the game reads your local data from. On Linux, this is in the `Documents` folder of the **prefix** you have configured to run XCOM2 in. On Windows, this is just in your user `Documents` folder. For WOTC, this should specifically point to `XCOM2 War of the Chosen/XComGame`.
- **Launch Command** - The command that is used to launch the game. I have `heroic` installed at `/usr/bin/heroic`, but if you're running the flatpak this will need to be either `flatpak` with `com.heroicgameslauncher.hgl` in the launch args, or a `.desktop` entry that runs Heroic.
- **Launch Args** - A list of strings that will be passed directly to the given launch command. For Heroic, along with any args you need to launch it (ergo flatpak), you'll want the values `--no-gui` and `heroic://launch?appName=1482002159&runner=gog` so that it will launch directly into XCOM2.

You can either set the game and local directories manually,
or allow LXCOMM to attempt to automatically find them for you.
Note that **LXCOMM will not work correctly if these directories are symlinks**.
If you've done this, you're probably way more qualified than I am to be making modifications to the game.

If you're on a similar setup to mine (Arch with Heroic binary installed, GOG version of XCOM2 installed), your launch command and launch args will want to look something like this.

|`/usr/bin/heroic`|  |
| - | - |
|key (optional) | `--no-gui` |
|key (optional) | `heroic://launch?appName=1482002159&runner=gog` |

Clicking on `Build` is technically unnecessary, but is useful for checking that there's nothing
wrong with your system before attempting to launch the game. The build command will take all of the information from your active profile and apply it to the directories you've provided.
Any files and folders that will be replaced are automatically moved to `filename.bak` if a file/folder with that name doesn't already exist.
The relevant configuration files and save data from your profile will be moved to the local directory,
while all the mods you've selected in the profile will be symlinked directly into your game's installation directory
(per [How To Install Mods Manually](https://www.reddit.com/r/xcom2mods/wiki/index/download_mods/#wiki_how_to_install_mods_manually)).
If all goes well and LXCOMM has proper file permissions, LXCOMM will display "Config was successfully applied". Once this has all been completed, hit `Launch` and hopefully,
after some waiting, you'll be greeted with `X2WOTCCommunityHighlander v[version] (hash)`
at the bottom right of the game's main menu.

If so, you've successfully launched a modded session of XCOM2! 🎉

At this point you should be ready to start doing more significant modifications.
Go crazy and break LXCOMM, bug reports and feedback are greatly appreciated.
Send those aliens crying back to their Elders, Commander.

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
- [ ] Per-profile character pools
- [x] Read most recent game logs
- [x] Recurse full mod dependency tree when checking for missing dependencies
- [ ] Support manually downloaded mods
- [x] Support workshop collections
- [ ] Copying/imporing+exporting profiles
- [ ] Disk cache trimming
- [x] ~~Ability to enter workshop ID directly for browsing or downloading (due to unreliable browsing results)~~ Search results should now be all-inclusive
- [ ] Investigate controller navigation in iced (or libcosmic) (for the Steam Deck gamers)

## License

lxcomm is dual-licensed under [MIT](LICENSE-MIT) or [Apache License 2.0](LICENSE-APACHE) at your choice.
