

default:
    @just --list

flatpak-vendor dest="dist/flatpak/config.toml":
    @rm -f {{dest}}
    cargo vendor-filterer --platform=x86_64-unknown-linux-gnu --platform=aarch64-unknown-linux-gnu --format=tar.gz --prefix=vendor >| {{dest}}
    # Set destination to vendor subdir
    sed -i -e 's/directory =.*/directory = ".\/lxcomm\/vendor"/g' {{dest}}

[working-directory("dist/flatpak")]
flatpak-build:
    flatpak-builder --force-clean --user --install-deps-from=flathub --repo=repo builddir io.github.phantomshift.lxcomm.json

[working-directory("dist/flatpak")]
flatpak-local-install:
    flatpak-builder --force-clean --user --install-deps-from=flathub --repo=repo --install builddir io.github.phantomshift.lxcomm.json

# Installation stuff
target-dir := env("CARGO_TARGET_DIR", "target")
app-name := "lxcomm"
profile := "release"
dest-root := "/usr"
dest-bin := dest-root / "bin"
dest-desktop := dest-root / "share" / "applications"
dest-icon := dest-root / "share" / "pixmaps"

desktop-file := dest-desktop / (app-name + ".desktop")
desktop-name := "LXCOMM"
desktop-comment := "Mod browser, downloader and manager for non-Steam XCOM2 (WOTC)"

build:
    cargo build --profile {{profile}}

# By default, this is a system-wide install
[linux]
install:
    install -Dm755 {{target-dir}}/{{profile}}/lxcomm {{dest-bin}}/{{app-name}}
    install -Dm644 assets/lxcomm.svg {{dest-icon}}/{{app-name}}.svg
    install -Dm644 dist/lxcomm.desktop {{desktop-file}}
    sed -i -e 's/Exec=lxcomm/Exec={{app-name}}/' {{desktop-file}}
    sed -i -e 's/Icon=lxcomm/Icon={{app-name}}/' {{desktop-file}}
    sed -i -E 's/Comment=.+/Comment={{desktop-comment}}/' {{desktop-file}}
    sed -i -e 's/Name=LXCOMM/Name={{desktop-name}}/' {{desktop-file}}

# For testing locally with dev build
[linux, private]
install-dev PROFILE="release":
    @just profile={{PROFILE}} build
    @just app-name="lxcomm_dev" \
                    dest-root="~/.local" \
                    desktop-comment="{{desktop-comment}} (Dev Build)" \
                    desktop-name="LXCOMM (Dev Build)" \
                    install


# Installs for user only
[linux]
install-local dest="~/.local": build
    @just dest-root={{dest}} install

[linux]
uninstall:
    rm {{dest-bin}}/{{app-name}}
    rm {{dest-desktop}}/{{app-name}}.desktop
    rm {{dest-icon}}/{{app-name}}.svg

# Remove files added by `install-local`
[linux]
uninstall-local dest="~/.local":
    @just dest-root={{dest}} uninstall
