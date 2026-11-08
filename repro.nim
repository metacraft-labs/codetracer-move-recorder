## Reprobuild dev env + build recipe for codetracer-move-recorder.
##
## Mirrors the dev shell declared in ``flake.nix`` (Linux/macOS) and
## the Windows DIY env declared in ``env.ps1``. ``repro build`` /
## ``repro test`` reproduce the same artefacts and the same test set
## that ``just build`` / ``just test`` produce today.
##
## Per ``codetracer-specs/Repo-Requirements.md`` §2.8 the recipe
## expresses build and test execution NATIVELY through typed-tool
## edges (`cargo.build`, `cargo.test`). It does NOT delegate to
## `shell(command = "bash scripts/...")` wrappers — delegation
## defeats the engine's incremental-build, action-cache, per-test
## invalidation, and the CI sharding the engine grows into per
## ``reprobuild-specs/CI-Sharding.md``.
##
## On Windows the recipe drives real reprobuild tool provisioning via
## the tarball entries the ``uses:`` packages declare (cargo, rustc,
## rustfmt, nim, nimble, capnp). On Linux/macOS the Nix flake
## continues to supply the same toolchain. Either path produces
## byte-equivalent build outputs and the same test pass/fail set —
## CI cross-checks this through the side-by-side `ci.yml` (nix) +
## `ci-reprobuild.yml` (reprobuild) flow per Repo-Requirements §2.9.
##
## Move: test corpus is pre-compiled Move bytecode artefacts.

import repro_project_dsl
import repro_dsl_stdlib/foreign_env
# The ``sui`` CLI the tests drive; reprobuild's stdlib has no package for it.
import sui_tools

# The owning constructor of ``ct-print``, which the tests decode every
# recorded trace with. It lives in the sibling checkout this repository's
# cargo ``path`` dependencies and tests already resolve at
# ``../codetracer-trace-format-nim``.
import "../codetracer-trace-format-nim/build_writer_artifacts"

const traceFormatNimRoot = "../codetracer-trace-format-nim"

package codetracer_move_recorder:
  defaultToolProvisioning (when defined(windows): tarball else: path)

  uses:
    # Rust toolchain — declared by version so the tarball-direct
    # provisioning entries in repro_dsl_stdlib/packages/cargo.nim /
    # rustc.nim / rustfmt.nim resolve on Windows. On Linux/macOS the
    # nix flake supplies the same versions.
    "rustc >=1.85"
    "cargo >=1.85"
    # C compiler driver — rustc links through `cc`, and build scripts
    # (cc-rs, the Nim FFI) compile C. Declaring it puts its directory on
    # every cargo edge's PATH. Windows links with MSVC instead, so its
    # cargo edges are not given a C compiler (see the tool refs at the end
    # of ``build:``).
    #
    # Windows still declares ``gcc``: the ct-print edge that
    # ``build_writer_artifacts.buildCtPrint`` constructs compiles with
    # ``nim c``'s gcc back-end on every host but macOS
    # (``CtPrintBackendSelectors``) and names ``gcc`` on its edge itself.
    # That ref provisions nothing unless the package declares the tool in
    # ``uses:`` (the module's "Native tool contract"; it is checked on POSIX
    # only), and without it the Windows ct-print compile fails with
    # ``Requested command not found: 'gcc.exe'``. The sibling's own recipe
    # declares the same ``gcc`` for its Windows build.
    when defined(macosx):
      "clang"
    else:
      "gcc"

    # Nim toolchain — codetracer_trace_writer_nim's build.rs compiles
    # a static library at cargo build time.
    "nim >=2.2 <3.0"
    "nimble"
    # nimble fetches the Nim writer's ``requires`` with git.
    "git"

    # Cap'n Proto schema compiler used by the recorder's build.rs.
    "capnp"

    # libzstd headers + library, needed when linking the Nim FFI
    # static library into the cargo build.
    "zstd"

    # zstd.h + libzstd for the sibling ct-print edge
    # (``build_writer_artifacts.buildCtPrint`` compiles and links against
    # them; ``zstd`` is the CLI only). The constructor cannot provision a
    # tool from inside ``build:`` and refuses a recipe that omits it.
    when not defined(windows):
      "zstd-dev"

    # Cap'n Proto is declared above; the writer's build.rs invokes capnp.

    # POSIX shell — drives the CLI-convention verification edge below,
    # the same ``bash tests/verify-cli-convention-no-silent-skip.sh``
    # step ``just test`` runs after ``cargo test`` (see ``Justfile``).
    # The script itself runs under bash and calls dirname and grep.
    # reprobuild's stdlib provisions dirname and grep from nixpkgs only; on
    # Windows they come with the PortableGit bash, in bash's own directory.
    "sh"
    "bash"
    when not defined(windows):
      "dirname"
      "grep"

    # The test run's replay tests (``tests/test_replay_temp_dirs.rs``, unix
    # only) stand in for the ``sui`` and ``aptos`` CLIs with ``/bin/sh``
    # scripts that run ``mkdir``, ``cp`` and ``basename``. Without them on
    # the edge's PATH the scripts cannot write their traces.
    when not defined(windows):
      "cp"
      "mkdir"
      "basename"

    # The Sui CLI: the Move tests compile their packages and capture their
    # traces with ``sui move test --trace`` and fail without it
    # (``tests/common/prerequisites.rs``). Declared in ``sui_tools.nim``.
    "sui"

    # pkg-config + OpenSSL — openssl-sys consults pkg-config to find
    # OpenSSL on Linux/macOS. The Windows build uses the rustls-tls
    # feature instead so neither is on the windows toolchain floor.
    when not defined(windows):
      "pkg-config"
      "openssl"
    # `choco pack` / `choco push` in .github/workflows/publish-chocolatey.yml.
    # Windows-guarded because Chocolatey is a Windows package manager with no
    # POSIX build, so an unguarded entry would fail to resolve on Linux/macOS.
    when defined(windows):
      "chocolatey"

  executable codetracerMoveRecorder:
    name: "codetracer-move-recorder"

  devEnv:
    when not defined(windows):
      useFlakeDevShell()
    activity "default"

  build:
    # ---- Primary build edge (the `default` collection) ----------------
    #
    # Native cargo build for the recorder binary. Enrolled into the
    # conventional ``default`` collection per
    # reprobuild-specs/Build-Graph-Collections.md §"`default`"; this
    # makes ``repro build`` (no positional target) materialise this
    # edge's closure.
    const binarySuffix = (when defined(windows): ".exe" else: "")
    const recorderBinary =
      "target/release/codetracer-move-recorder" & binarySuffix

    let recorderBuild = cargo.build(
      locked = true,
      release = true,
      actionId = "codetracer-move-recorder.cargo-build",
      extraInputs = @[
        "Cargo.toml", "Cargo.lock",
        "src", "build.rs"
      ],
      extraOutputs = @[recorderBinary])
    discard collect("default", @[recorderBuild])

    # ---- Test-binary build + run edges (the `test` collection) -------
    #
    # Two-stage shape per Repo-Requirements.md §2.8: `cargo.test(noRun =
    # true)` builds every cargo test binary into
    # `target/debug/deps/<crate>-<hash>` (the engine tracks the deps
    # directory as the build edge's effect set because the hashed
    # filename floats with input content); `cargo.test(noRun = false)`
    # then runs the binaries in one cargo invocation. The execute edge
    # depends on the build edge so the engine only re-runs tests when
    # an input changed since the last successful execution.
    #
    # Per-test execute edges fall out automatically once the
    # ct-test-runner cargo adapter lands per
    # reprobuild-specs/Test-Edges-And-Parallel-Runner.milestones.org
    # §M4 — the whole-binary edge becomes a fan-out point without
    # changing this recipe.

    let testsBuild = cargo.test(
      locked = true,
      noRun = true,
      actionId = "codetracer-move-recorder.cargo-test-build",
      extraInputs = @[
        "Cargo.toml", "Cargo.lock",
        "src", "build.rs", "tests"
      ],
      extraOutputs = @["target/debug/deps"])

    # ``ct-print`` prerequisite: the tests decode their traces with
    # ``ct-print`` at the fixed sibling path
    # ``../codetracer-trace-format-nim/ct-print`` and FAIL when it is
    # missing. ``just test`` builds it first (``just build-ct-print``); here
    # it is an edge of the graph, constructed by the sibling's own
    # ``buildCtPrint`` so the compile flags stay in the repository that
    # owns them. The test-run edge orders after it and names the binary as
    # an input, so a missing or rebuilt ``ct-print`` re-runs the tests.
    #
    # It runs after the test build because that build's
    # ``codetracer_trace_writer_nim`` ``build.rs`` is what installs the
    # sibling's nimble ``requires`` (``results``, ``stew``) into the nimble
    # path the ct-print compile resolves them from. ``just build-ct-print``
    # gets the same packages from ``nimble buildCtPrint``.
    let ctPrintBuild = buildCtPrint(traceFormatNimRoot,
                                    after = @[testsBuild.action])

    let testsRun = cargo.test(
      locked = true,
      actionId = "codetracer-move-recorder.cargo-test-run",
      after = @[testsBuild.action, ctPrintBuild],
      extraInputs = @[
        "Cargo.toml", "Cargo.lock",
        "src", "tests", "test-programs",
        "target/debug/deps",
        ctPrintPath(traceFormatNimRoot)
      ])

    # ---- CLI-convention verification edge -----------------------------
    #
    # ``just test`` runs ``bash
    # tests/verify-cli-convention-no-silent-skip.sh`` after ``cargo
    # test``. The script asserts the recorder's ``--help`` / ``--version``
    # surface complies with the recorder CLI conventions (``ct print``
    # present, ``--format`` / ``CODETRACER_FORMAT`` absent, ``--out-dir``
    # present, the ``CODETRACER_MOVE_RECORDER_*`` env knobs referenced in
    # ``src/``). It is not a cargo target, so it is modelled as its own
    # ``sh.shell`` execute edge rather than dropped — reproducing the
    # repo's full ``just test`` set. The script builds the debug binary
    # and inspects its ``--help`` text, so it is re-run every ``repro
    # test`` pass (matching ``just test``); ``after`` the cargo test-build
    # edge guarantees the binary exists before the script runs.
    let cliVerify = shell(
      command = "bash tests/verify-cli-convention-no-silent-skip.sh",
      actionId = "codetracer-move-recorder.verify-cli-convention",
      after = @[testsBuild.action],
      extraInputs = @[
        "tests/verify-cli-convention-no-silent-skip.sh",
        "Cargo.toml", "Cargo.lock", "src"
      ],
      cacheable = false)

    # Attach the tools each edge runs. An edge receives only the tools named
    # on it (plus the sub-tools of the typed constructor that built it): the
    # cargo edges run rustc, the C compiler, and the build scripts' nim,
    # nimble, git, capnp and zstd; the CLI-convention script runs ``cargo
    # build`` itself; the test run also drives ``sui``, and on POSIX the
    # fake ``sui`` / ``aptos`` scripts of its replay tests run ``sh``,
    # ``mkdir``, ``cp`` and ``basename``. Path provisioning also passes the
    # caller's PATH through, so a ``repro test`` run from a shell that has
    # coreutils (the dev shell, a workstation) hides an omission here; the CI
    # runner and Windows tarball provisioning do not. (CI
    # once reported "sui replay exited successfully but wrote no trace"
    # because ``cp`` was missing from this edge.)
    for action in [recorderBuild, testsBuild.action, testsRun.action, cliVerify]:
      appendRegisteredActionToolIdentityRefs(action.id,
        ["cargo", "rustc", "nim", "nimble", "git", "capnp", "zstd"])
      when defined(linux):
        appendRegisteredActionToolIdentityRefs(action.id,
          ["gcc", "pkg-config", "openssl"])
      elif defined(macosx):
        appendRegisteredActionToolIdentityRefs(action.id,
          ["clang", "pkg-config", "openssl"])
    appendRegisteredActionToolIdentityRefs(testsRun.action.id, ["sui"])
    when not defined(windows):
      appendRegisteredActionToolIdentityRefs(testsRun.action.id,
        ["sh", "mkdir", "cp", "basename"])
    appendRegisteredActionToolIdentityRefs(cliVerify.id, ["sh", "bash"])
    when not defined(windows):
      appendRegisteredActionToolIdentityRefs(cliVerify.id, ["dirname", "grep"])

    discard collect("test", @[testsRun.action, cliVerify])
