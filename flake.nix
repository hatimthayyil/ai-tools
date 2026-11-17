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
                ./tools/convert
              ];
            };
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [
              pkgs.makeBinaryWrapper
              pkgs.pkg-config
            ];
            # nixpkgs' cargo hook has -sys crates link system libraries, found through pkg-config:
            # aws-lc for rustls' crypto (via genai's reqwest), sqlite for rusqlite.
            buildInputs = [
              pkgs.aws-lc
              pkgs.sqlite
            ];
            # git: ai runs git gc on ~/ai. restic: ai backup runs it. A git or restic of the user's
            # own on PATH comes first.
            postInstall = ''
              mkdir -p $out/share/ai
              cp -r --no-preserve=mode ${./plugins} $out/share/ai/plugins
              wrapProgram $out/bin/ai --suffix PATH : ${
                lib.makeBinPath [
                  pkgs.gitMinimal
                  pkgs.restic
                ]
              }
            '';
            # cacert: the API backend's HTTP client loads the system's CA certificates, even in tests.
            nativeCheckInputs = [
              pkgs.git
              pkgs.cacert
              pkgs.restic
            ];
            nativeInstallCheckInputs = [ pkgs.versionCheckHook ];
            doInstallCheck = true;
            postInstallCheck = ''
              test -f $out/share/ai/plugins/.claude-plugin/marketplace.json
              test -f $out/share/ai/plugins/claude/.claude-plugin/plugin.json
              test -f $out/share/ai/plugins/claude/prompts/recall.txt
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

      homeModules.default = import ./nix/home.nix self;
    };
}
