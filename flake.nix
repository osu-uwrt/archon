{
  description = "archon development environment (from the shared UWRT Nix setup)";

  # TODO: switch to the team's repo when uwrt_nix moves there
  inputs.uwrt.url = "github:atk422/uwrt_nix";

  outputs =
    { uwrt, ... }:
    {
      devShells = uwrt.lib.forAllSystems (
        pkgs:
        let
          # winit and the GL renderer load these at runtime rather than linking them, so they go on LD_LIBRARY_PATH
          runtimeLibs = pkgs.lib.optionals pkgs.stdenv.isLinux (
            with pkgs;
            [
              libGL
              libxkbcommon
              wayland
              libx11
              libxcursor
              libxi
              libxrandr
              # For kittest's headless renderer (cargo test -- --ignored screenshots)
              vulkan-loader
            ]
          );
        in
        {
          default = pkgs.mkShell {
            # ROS 2 Humble with the riptide messages, for the ROS backend
            inputsFrom = [ uwrt.devShells.${pkgs.stdenv.hostPlatform.system}.ros ];
            # libclang and the C headers for bindgen, which generates the rcl bindings
            nativeBuildInputs = [ pkgs.rustPlatform.bindgenHook ];
            packages = [
              (pkgs.rust-bin.stable.latest.default.override {
                extensions = [
                  "rust-src"
                  "clippy"
                  "rustfmt"
                ];
              })
              pkgs.pkg-config
            ];
            # Added to the ROS libraries already there rather than replacing them
            shellHook = ''
              export LD_LIBRARY_PATH=${pkgs.lib.makeLibraryPath runtimeLibs}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
            '';
          };
        }
      );
    };
}
