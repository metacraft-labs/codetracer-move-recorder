## The ``sui`` CLI, as a tool the test edges can name.
##
## The Move tests compile their packages with ``sui move test --trace`` and
## require ``sui --version`` to answer (``tests/common/prerequisites.rs``);
## a missing ``sui`` fails them. reprobuild's stdlib has no ``sui`` package,
## so this repository declares the one it already depends on:
##
## * Linux / macOS: the ``sui`` package of this flake's ``mcl-blockchain``
##   input (``tools/sui/default.nix``), the same derivation the dev shell
##   provides. Its lock identity hashes the expression and the flake files,
##   so a ``flake.lock`` bump re-provisions it.
## * Windows: MystenLabs' official prebuilt release, the same archive and
##   version ``env.ps1`` installs (``$suiVersion``). Keep the two equal; the
##   ``test_repro_sui_pin_matches_env_ps1`` test compares this file's version
##   and URL against ``env.ps1``'s pin.
import blake3
import repro_project_dsl

const suiExpressionSources = staticRead("tools/sui/default.nix") & "\0" &
  staticRead("flake.nix") & "\0" & staticRead("flake.lock")
let suiExpressionIdentity =
  blake3.toHex(blake3.digest(suiExpressionSources))

package sui:
  provisioning:
    nixPackage "sui", executablePath = "bin/sui",
      expressionFile = "tools/sui/default.nix",
      lockIdentity = "owning-move-flake-sui:" & suiExpressionIdentity
    # https://github.com/MystenLabs/sui/releases/tag/mainnet-v1.82.3 -- the
    # archive holds the release binaries (sui.exe among them) at its root.
    tarball url = "https://github.com/MystenLabs/sui/releases/download/mainnet-v1.82.3/sui-mainnet-v1.82.3-windows-x86_64.tgz",
      sha256 = "d9d7c6887ff44f3ee957b87ab506669bab6aaa638f491cf2a9024ae720edaf8a",
      archiveType = "tar.gz",
      executablePath = "sui.exe",
      version = "1.82.3",
      packageId = "sui@1.82.3",
      cpu = "x86_64",
      os = "windows",
      lockIdentity = "tarball:sui@1.82.3:sha256:d9d7c6887ff44f3ee957b87ab506669bab6aaa638f491cf2a9024ae720edaf8a"
