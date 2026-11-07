# The `sui` CLI the tests drive, taken from this repository's own flake: the
# `sui` package of its `mcl-blockchain` input (nix-blockchain-development),
# at the revision `flake.lock` pins. It is the same `sui` the dev shell
# (`nix develop`) puts on PATH, so `repro test` and `just test` run the
# tests against one binary. Read by `sui_tools.nim`.
{
  system ? builtins.currentSystem,
}:
let
  projectRoot = ../..;
  # Only flake.nix and flake.lock: the expression must not depend on (and
  # copy) the rest of the checkout.
  source = builtins.path {
    path = projectRoot;
    name = "move-recorder-owning-flake-source";
    filter =
      path: type:
      path == toString projectRoot
      || (
        type == "regular"
        && builtins.elem (builtins.baseNameOf path) [
          "flake.nix"
          "flake.lock"
        ]
      );
  };
  owner = builtins.getFlake (builtins.unsafeDiscardStringContext ("path:" + toString source));
in
assert builtins.elem system [
  "x86_64-linux"
  "aarch64-linux"
  "x86_64-darwin"
  "aarch64-darwin"
];
owner.inputs.mcl-blockchain.packages.${system}.sui
