

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