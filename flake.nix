{
  description = "ai: all-in-one AI tool";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      inherit (nixpkgs) lib;
      forAllSystems = lib.genAttrs [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          ai = pkgs.rustPlatform.buildRustPackage {
            pname = "ai";
            version = (lib.importTOML ./Cargo.toml).package.version;
            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [
                ./Cargo.toml
                ./Cargo.lock
                ./src
                ./tests
              ];
            };
            cargoLock.lockFile = ./Cargo.lock;
            postInstall = ''
              mkdir -p $out/share/ai
              cp -r --no-preserve=mode ${./plugins} $out/share/ai/plugins
            '';
            # cacert: the API backend's HTTP client loads the system's CA certificates, even in tests.
            nativeCheckInputs = [
              pkgs.git
              pkgs.cacert
            ];
            nativeInstallCheckInputs = [ pkgs.versionCheckHook ];
            doInstallCheck = true;
            postInstallCheck = ''
              test -f $out/share/ai/plugins/.claude-plugin/marketplace.json
              test -f $out/share/ai/plugins/claude/.claude-plugin/plugin.json
            '';
            meta = {
              description = "All-in-one AI tool: permanent memory for AI agents";
              homepage = "https://github.com/hatimthayyil/ai-tools";
              license = lib.licenses.cc0;
              mainProgram = "ai";
              platforms = lib.platforms.unix;
            };
          };
        in
        {
          inherit ai;
          default = ai;
        }
      );
    };
}
