# shellcheck shell=bash

gleamConfigHook() {
    echo "Executing gleamConfigHook"

    if [ -z "${gleamHexDeps-}" ]; then
        echo
        echo "ERROR: gleamHexDeps is not set"
        echo "Hint: set gleamHexDeps to the output of fetchHexDeps, or pass 'manifest' to buildGleamPackage."
        echo
        exit 1
    fi

    export HOME="$TMPDIR"
    mkdir -p "$HOME/.cache"

    echo "Populating Gleam hex cache from Nix store"
    cp -r "$gleamHexDeps/gleam" "$HOME/.cache/gleam"
    chmod -R u+w "$HOME/.cache/gleam"

    # If the consumer wants Lustre dev tools to use system bun (from PATH)
    # instead of downloading its own copy (which fails in the Nix sandbox),
    # append the config to gleam.toml.
    if [ -n "${gleamUseSystemBun-}" ]; then
        echo "Configuring Lustre to use system bun"
        echo "" >> gleam.toml
        echo '[tools.lustre.bin]' >> gleam.toml
        echo 'bun = "system"' >> gleam.toml
    fi

    echo "Downloading Gleam dependencies (offline from hex cache)"
    @gleam@/bin/gleam deps download

    echo "Finished gleamConfigHook"
}

postPatchHooks+=(gleamConfigHook)
