{{CORA_SKILL_MARKER}}

# Installing cora

Install cora from the GitHub release binary. Do not build it from source to use it.

## Why not build it

The release binary contains the code embedding model (about 69 MB on disk, 38 MB archive) and works fully offline. A build from source (`cargo build`, `cargo install`) does not contain the model: cora downloads it on first use, which fails in offline or sandboxed agent environments and silently turns the meaning-based search off. Build from source only when you are changing cora itself.

## Check first

Run `cora --version`. If it prints a version, you are done unless the project asks for a newer one. If the command is missing, install it.

## Install (Linux, macOS)

```bash
curl -fsSL https://raw.githubusercontent.com/codecoradev/cora-code/main/install.sh | sh
```

The script picks the archive for your platform, verifies its SHA-256 checksum (and the cosign signature when `cosign` is installed) and puts `cora` in `~/.local/bin`. Make sure that directory is on `PATH`. Pin a version with `CORA_VERSION=vX.Y.Z` in front of `sh`, change the folder with `CORA_INSTALL_DIR`, and make a missing signature fatal with `CORA_REQUIRE_SIGNATURE=1`.

## Install by hand (any platform)

Download the archive for your platform and `checksums-sha256.txt` from the project's GitHub Releases page, check it with `sha256sum -c --ignore-missing checksums-sha256.txt`, unpack it and put `cora` on `PATH`. Windows ships a `.zip`.

## After installing or upgrading

1. `cora install` registers the hooks, the MCP server and these skills for your agents, and refreshes the copies cora wrote earlier.
2. `cora index` in the project root. An upgrade can change what is ignored, so index again.
3. In an offline or sandboxed environment, set `CORA_EMBED_OFFLINE=1`: the bundled model is unpacked locally, nothing is downloaded.
