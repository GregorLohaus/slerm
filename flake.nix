{
  description = "slerm: a Ghostty-powered Rust terminal";

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
            name = "slerm-ghostty-zig-deps";
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
          # wgpu dlopens the Vulkan loader and libEGL, which in turn dlopen the GPU
          # vendor driver named in the ICD / EGL vendor JSON. Nixpkgs supplies the
          # loaders but never the driver: that lives outside /nix/store, under
          # /run/opengl-driver on NixOS and in the distro's library directory
          # everywhere else. With no driver reachable the loaders enumerate nothing
          # and wgpu fails with "Failed to create surface for any enabled backend: {}".
          #
          # The host directories cannot simply go on LD_LIBRARY_PATH: it outranks the
          # binary's DT_RUNPATH, so the host's libc.so.6 / libgcc_s.so.1 would load
          # into a process linked against the store's glibc and segfault on startup.
          # Instead, link only the vendor driver objects into a private directory and
          # expose that. Their remaining dependencies (libc, libX11, ...) then resolve
          # against the already-loaded store libraries.
          hostDriverGlobs = [
            "libGLX_*.so*" # GLX vendor drivers (glvnd dispatch stays from the store)
            "libEGL_*.so*"
            "libGLESv1_CM_*.so*"
            "libGLESv2_*.so*"
            "libnvidia-*.so*"
            "libcuda.so*"
            "libvulkan_*.so*" # mesa's radv / anv, not the loader itself
            "libgallium*.so*"
            "*_dri.so"
            "libdrm*.so*"
          ];
          hostLibraryDirs = [
            "/usr/lib/${pkgs.stdenv.hostPlatform.parsed.cpu.name}-linux-gnu"
            "/usr/lib64"
            "/usr/lib"
          ];
          # Runs inside the wrapper, before exec.
          collectHostDrivers = ''
            if [ -d /run/opengl-driver/lib ]; then
              export LD_LIBRARY_PATH="''${LD_LIBRARY_PATH:+$LD_LIBRARY_PATH:}/run/opengl-driver/lib"
            elif [ -z "''${SLERM_NO_HOST_DRIVERS:-}" ]; then
              slermCache="''${XDG_CACHE_HOME:-$HOME/.cache}/slerm/gpu-drivers"
              slermStale=1
              if [ -e "$slermCache/.stamp" ]; then
                slermStale=0
                for slermDir in ${lib.concatStringsSep " " hostLibraryDirs}; do
                  if [ -d "$slermDir" ] && [ "$slermDir" -nt "$slermCache/.stamp" ]; then
                    slermStale=1
                  fi
                done
              fi
              if [ "$slermStale" = 1 ]; then
                slermTmp="$slermCache.$$"
                rm -rf "$slermTmp" && mkdir -p "$slermTmp" || slermTmp=""
                for slermDir in ${lib.concatStringsSep " " hostLibraryDirs}; do
                  [ -n "$slermTmp" ] && [ -d "$slermDir" ] || continue
                  for slermGlob in ${lib.concatStringsSep " " hostDriverGlobs}; do
                    for slermLib in "$slermDir"/$slermGlob; do
                      [ -e "$slermLib" ] || continue
                      ln -sfn "$slermLib" "$slermTmp/''${slermLib##*/}"
                    done
                  done
                done
                if [ -n "$slermTmp" ]; then
                  : > "$slermTmp/.stamp"
                  rm -rf "$slermCache" && mv "$slermTmp" "$slermCache" || rm -rf "$slermTmp"
                fi
              fi
              [ -d "$slermCache" ] &&
                export LD_LIBRARY_PATH="''${LD_LIBRARY_PATH:+$LD_LIBRARY_PATH:}$slermCache"
              unset slermCache slermStale slermTmp slermDir slermGlob slermLib
            fi
          '';
          # macOS only gives a process a dock icon, a menu bar and a real
          # application name when it runs from a bundle, so ship one and keep
          # bin/slerm as a symlink into it for terminal use.
          appBundle = version: ''
            app="$out/Applications/slerm.app"
            mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
            cat > "$app/Contents/Info.plist" <<EOF
            <?xml version="1.0" encoding="UTF-8"?>
            <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
            <plist version="1.0">
            <dict>
              <key>CFBundleName</key><string>slerm</string>
              <key>CFBundleDisplayName</key><string>slerm</string>
              <key>CFBundleExecutable</key><string>slerm</string>
              <key>CFBundleIconFile</key><string>slerm.icns</string>
              <key>CFBundleIdentifier</key><string>org.slerm.slerm</string>
              <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
              <key>CFBundlePackageType</key><string>APPL</string>
              <key>CFBundleShortVersionString</key><string>${version}</string>
              <key>CFBundleVersion</key><string>${version}</string>
              <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
              <key>LSMinimumSystemVersion</key><string>11.0</string>
              <key>NSHighResolutionCapable</key><true/>
              <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
            </dict>
            </plist>
            EOF
            cp ${./assets/slerm.icns} "$app/Contents/Resources/slerm.icns"
            mv "$out/bin/slerm" "$app/Contents/MacOS/slerm"
            ln -s "$app/Contents/MacOS/slerm" "$out/bin/slerm"
          '';
          ghosttyEnv = {
            GHOSTTY_SOURCE_DIR = "${ghostty-src}";
            GHOSTTY_ZIG_SYSTEM_DIR = "${ghosttyDeps}";
            LIBGHOSTTY_VT_SYS_OPTIMIZE = "ReleaseFast";
          };
          makeIcon = pkgs.writeShellApplication {
            name = "slerm-make-icon";
            runtimeInputs = [ pkgs.resvg ];
            text = ''
              if [ ! -f assets/slerm.svg ]; then
                echo "Run nix run .#make-icon from the slerm repository root." >&2
                exit 1
              fi
              mkdir -p target/slerm.iconset
              resvg --width 1024 --height 1024 assets/slerm.svg assets/slerm.png
              for size in 16 32 128 256 512; do
                resvg --width "$size" --height "$size" assets/slerm.svg \
                  "target/slerm.iconset/icon_''${size}x''${size}.png"
                double=$((size * 2))
                resvg --width "$double" --height "$double" assets/slerm.svg \
                  "target/slerm.iconset/icon_''${size}x''${size}@2x.png"
              done
              # iconutil is part of macOS itself; resvg is supplied by Nix.
              /usr/bin/iconutil -c icns target/slerm.iconset -o assets/slerm.icns
              echo "Generated assets/slerm.png and assets/slerm.icns from assets/slerm.svg"
            '';
          };
          slerm = pkgs.rustPlatform.buildRustPackage (
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
                  ./assets
                  # Embedded runtime and the bindings checked by Rust tests.
                  ./sdk/runtime.js
                  ./sdk/src/generated.ts
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
                "--skip=pane::scrollback_tests::"
                "--skip=tests::pty_executes_input_and_bracketed_paste_is_sanitized"
                "--skip=tests::new_panes_start_in_the_directory_of_the_pane_they_came_from"
                "--skip=tests::osc7_wins_over_the_process_directory_but_only_for_this_machine"
              ];

              postInstall = lib.optionalString pkgs.stdenv.hostPlatform.isDarwin (
                appBundle manifest.package.version
              );

              postFixup = lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
                wrapProgram "$out/bin/slerm" \
                  --suffix LD_LIBRARY_PATH : ${lib.makeLibraryPath runtimeLibraries} \
                  --run ${lib.escapeShellArg collectHostDrivers}
              '';

              meta = {
                description = manifest.package.description;
                license = lib.licenses.mit;
                mainProgram = "slerm";
                platforms = systems;
              };
            }
            // ghosttyEnv
          );
        in
        {
          packages = {
            default = slerm;
            slerm = slerm;
          }
          // lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin { make-icon = makeIcon; };
          devShells.default = pkgs.mkShell (
            {
              inputsFrom = [ slerm ];
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
                  ${collectHostDrivers}
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
      apps = forAllSystems (
        system:
        {
          default = {
            type = "app";
            program = "${self.packages.${system}.default}/bin/slerm";
            meta.description = "Run slerm";
          };
        }
        // lib.optionalAttrs nixpkgs.legacyPackages.${system}.stdenv.hostPlatform.isDarwin {
          make-icon = {
            type = "app";
            program = "${self.packages.${system}.make-icon}/bin/slerm-make-icon";
            meta.description = "Regenerate slerm's macOS icons from the editable SVG";
          };
        }
      );
      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt);
    };
}
