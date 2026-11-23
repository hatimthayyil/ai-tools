{
  description = "ai: all-in-one AI tool";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
    }:
    let
      inherit (nixpkgs) lib;
      forAllSystems = lib.genAttrs [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      version = (lib.importTOML ./Cargo.toml).package.version;
      # Cargo.lock with ai's own version zeroed, for the dependencies' build.
      lockUnversioned =
        let
          text = builtins.unsafeDiscardStringContext (builtins.readFile ./Cargo.lock);
          entry = v: ''
            name = "ai-tools"
            version = "${v}"'';
          zeroed = builtins.replaceStrings [ (entry version) ] [ (entry "0.0.0") ] text;
        in
        assert lib.assertMsg (zeroed != text) "flake.nix: Cargo.lock has no ai-tools ${version}";
        builtins.toFile "Cargo.lock" zeroed;
      perSystem =
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          craneLib = crane.mkLib pkgs;
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
          common = {
            inherit src;
            pname = "ai";
            cargoVendorDir = craneLib.vendorCargoDeps { inherit src; };
            strictDeps = true;
            nativeBuildInputs = [ pkgs.pkg-config ];
            # aws-lc-sys, rustls' crypto (via genai's reqwest), links the AWS-LC pkg-config finds.
            buildInputs = [ pkgs.aws-lc ];
          };
          # The dependencies, built once for the package and the checks. Their build sees ai as
          # version 0.0.0, in Cargo.toml and in Cargo.lock, so a release reuses it. pname, version
          # and cargoVendorDir are given, so that evaluation reads nothing back from a build.
          cargoArtifacts = craneLib.buildDepsOnly (
            common
            // {
              src = null;
              version = "0.0.0";
              dummySrc = craneLib.mkDummySrc {
                src = lib.fileset.toSource {
                  root = ./.;
                  fileset = ./Cargo.toml;
                };
                cargoLock = lockUnversioned;
                cleanCargoTomlFilter =
                  path:
                  craneLib.filters.cargoTomlDefault path
                  &&
                    path != [
                      "package"
                      "version"
                    ];
              };
            }
          );
          ai = craneLib.buildPackage (
            common
            // {
              inherit version cargoArtifacts;
              # checks.tests runs the tests.
              doCheck = false;
              nativeBuildInputs = common.nativeBuildInputs ++ [ pkgs.makeBinaryWrapper ];
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
              nativeInstallCheckInputs = [ pkgs.versionCheckHook ];
              doInstallCheck = true;
              postInstallCheck = ''
                test -f $out/share/ai/plugins/.claude-plugin/marketplace.json
                test -f $out/share/ai/plugins/claude/.claude-plugin/plugin.json
                test -f $out/share/ai/plugins/claude/prompts/recall.txt
                test -f $out/share/ai/plugins/claude/prompts/fold.txt
              '';
              meta = {
                description = "All-in-one AI tool: permanent memory for AI agents";
                homepage = "https://github.com/hatimthayyil/ai-tools";
                license = lib.licenses.cc0;
                mainProgram = "ai";
                platforms = lib.platforms.unix;
              };
            }
          );
          # cacert: the API backend's HTTP client loads the system's CA certificates, even in tests.
          tests = craneLib.cargoTest (
            common
            // {
              inherit version cargoArtifacts;
              nativeCheckInputs = [
                pkgs.git
                pkgs.cacert
                pkgs.restic
              ];
            }
          );
        in
        {
          packages = {
            inherit ai;
            default = ai;
          };
          checks = { inherit tests; };
        };
    in
    {
      packages = forAllSystems (system: (perSystem system).packages);

      checks = forAllSystems (system: (perSystem system).checks);

      homeModules.default = import ./nix/home.nix self;
    };
}
