# lxcomm - Linux (XCOM2) Mod Manager

<div align="center">

![icon](assets/lxcomm_icon.svg)

</div>

Mod browser and downloader made specifically for usage with the GOG version of XCOM2(WOTC) on Linux.

## Tracking Ideas

In no particular order:

- [x] Option to log out of steamcmd on exit (default command: quit)
- [x] Text editor + diffing for per-profile mod ini settings
- [x] On detailed view open, resolve unknown dependencies in the background
- [x] Filter library with option for [fuzzy finding](https://github.com/Blakeinstein/fuse-rust)
- [ ] Check for updates without having to invoke download (build manifest file from scratch and then try running `steamcmd +workshop_status`?)
- [ ] Automatically find game installations and folders (using [walkdir](https://github.com/BurntSushi/walkdir)?)
- [ ] Options for ranking search results in browsing
- [ ] Per-profile character pools (user setup, point at Documents/My Games/XCOM2 War of the Chosen/XComGame/CharacterPool)
- [ ] Read most recent game logs (user setup, point at Documents/My Games/XCOM2 War of the Chosen/XComGame/Logs/Launch.log)
- [ ] Support manually downloaded mods

## License

lxcomm is dual-licensed under [MIT](LICENSE-MIT) or [Apache License 2.0](LICENSE-APACHE) at your choice.
