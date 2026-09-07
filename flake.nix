{
  description = "Ghostty-powered Rust terminal";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    ghostty-src = {
      # Must match libghostty-vt-sys in Cargo.lock.
      url = "github:ghostty-org/ghostty/a887df42c56f6de86c0fe6da9c4eeca37931e083";
      flake = false;
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      ghostty-src,
    }:
    let
      inherit (nixpkgs) lib;
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forAllSystems = lib.genAttrs systems;
      perSystem = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          manifest = builtins.fromTOML (builtins.readFile ./Cargo.toml);
          zig = pkgs.zig_0_15;
          zigForCargo = pkgs.writeShellScriptBin "zig" ''
            if [ "$1" = build ]; then
              shift
              exec ${lib.getExe zig} build -Dcpu=baseline "-j''${NIX_BUILD_CORES:-1}" "$@"
            fi
            exec ${lib.getExe zig} "$@"
          '';
          ghosttyDeps = pkgs.callPackage "${ghostty-src}/build.zig.zon.nix" {
            name = "rust-term-ghostty-zig-deps";
            zig_0_15 = zig;
            # Zig 0.15's build runner needs real directories, not a symlink farm.
            linkFarm =
              name: entries:
              pkgs.runCommand name { } ''
                mkdir -p "$out"
                ${lib.concatMapStringsSep "\n" (entry: ''
                  cp -rL ${entry.path} "$out"/${lib.escapeShellArg entry.name}
                '') entries}
              '';
          };
          runtimeLibraries = lib.optionals pkgs.stdenv.hostPlatform.isLinux (
            with pkgs;
            [
              libx11
              libxcursor
              libxi
              libxrandr
              libxrender
              libxcb
              libxkbcommon
              wayland
              libGL
              vulkan-loader
            ]
          );
          ghosttyEnv = {
            GHOSTTY_SOURCE_DIR = "${ghostty-src}";
            GHOSTTY_ZIG_SYSTEM_DIR = "${ghosttyDeps}";
            LIBGHOSTTY_VT_SYS_OPTIMIZE = "ReleaseFast";
          };
          rustTerm = pkgs.rustPlatform.buildRustPackage (
            {
              pname = manifest.package.name;
              version = manifest.package.version;
              src = lib.fileset.toSource {
                root = ./.;
                fileset = lib.fileset.unions [
                  ./Cargo.toml
                  ./Cargo.lock
                  ./config.example.toml
                  ./src
                ];
              };
              cargoLock.lockFile = ./Cargo.lock;
              strictDeps = true;

              nativeBuildInputs = [
                zig
                pkgs.pkg-config
              ]
              ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.makeWrapper ]
              ++ lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
                pkgs.darwin.cctools
                pkgs.xcbuild
              ];
              buildInputs = runtimeLibraries;

              # Cargo owns the phases; Zig is invoked by libghostty-vt-sys.
              dontUseZigConfigure = true;
              dontUseZigBuild = true;
              dontUseZigCheck = true;
              dontUseZigInstall = true;

              preBuild = ''
                export PATH="${zigForCargo}/bin:$PATH"
              '';

              # PTY tests require a host shell and PTY devices outside the sandbox.
              checkFlags = [
                "--test-threads=1"
                "--skip=app::tests::"
                "--skip=tests::pty_executes_input_and_bracketed_paste_is_sanitized"
              ];

              postFixup = lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
                wrapProgram "$out/bin/rust-term" \
                  --suffix LD_LIBRARY_PATH : ${lib.makeLibraryPath runtimeLibraries}
              '';

              meta = {
                description = manifest.package.description;
                license = lib.licenses.mit;
                mainProgram = "rust-term";
                platforms = systems;
              };
            }
            // ghosttyEnv
          );
        in
        {
          packages = {
            default = rustTerm;
            rust-term = rustTerm;
          };
          devShells.default = pkgs.mkShell (
            {
              inputsFrom = [ rustTerm ];
              packages = [
                pkgs.cargo
                pkgs.rustc
                pkgs.rustfmt
                pkgs.clippy
                zig
              ];
              shellHook = ''
                export ZIG_GLOBAL_CACHE_DIR="$PWD/target/zig-global-cache"
                ${lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
                  export LD_LIBRARY_PATH="${lib.makeLibraryPath runtimeLibraries}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
                ''}
              '';
            }
            // ghosttyEnv
          );
        }
      );
    in
    {
      packages = forAllSystems (system: perSystem.${system}.packages);
      devShells = forAllSystems (system: perSystem.${system}.devShells);
      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/rust-term";
          meta.description = "Run rust-term";
        };
      });
      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt);
    };
}
