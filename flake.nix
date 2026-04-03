{
  description = "Bitcoin Indexer — ordinals, BRC-20, and runes indexer for PostgreSQL";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        # ── Rust indexer binary ──
        # Use clang stdenv — rocksdb C++ compilation fails with gcc 15
        clangStdenv = pkgs.llvmPackages_18.stdenv;
        rustPlatformClang = pkgs.makeRustPlatform {
          rustc = pkgs.rustc;
          cargo = pkgs.cargo;
          stdenv = clangStdenv;
        };

        bitcoin-indexer = rustPlatformClang.buildRustPackage {
          pname = "bitcoin-indexer";
          version = "3.0.0";
          src = ./.;

          cargoLock = {
            lockFile = ./Cargo.lock;
            outputHashes = {
              "schemars-0.8.16" = "sha256-xg7TUTxo+7vDSOQQuWkTl0ajcvO9iP9IP8x8uWUcFqM=";
            };
          };

          nativeBuildInputs = with pkgs; [
            pkg-config
            llvmPackages_18.clang
            llvmPackages_18.llvm
            rustPlatform.bindgenHook
          ];

          buildInputs = with pkgs; [
            openssl
            snappy
            gflags
            zlib
            bzip2
            lz4
            zstd
            libunwind
          ];

          # Let librocksdb-sys compile its bundled rocksdb 9.9.3 from source.
          # The clang stdenv ensures clang is used instead of gcc 15 (which breaks rocksdb C++).

          buildFeatures = [ "release" ];

          # Force clang for ALL C/C++ compilation including cc-rs (rocksdb)
          LIBCLANG_PATH = "${pkgs.llvmPackages_18.libclang.lib}/lib";
          CC = "${pkgs.llvmPackages_18.clang}/bin/clang";
          CXX = "${pkgs.llvmPackages_18.clang}/bin/clang++";
          # cc-rs uses TARGET_CC/TARGET_CXX to find the compiler
          TARGET_CC = "${pkgs.llvmPackages_18.clang}/bin/clang";
          TARGET_CXX = "${pkgs.llvmPackages_18.clang}/bin/clang++";
          HOST_CC = "${pkgs.llvmPackages_18.clang}/bin/clang";
          HOST_CXX = "${pkgs.llvmPackages_18.clang}/bin/clang++";

          doCheck = false; # Tests require a running bitcoind + postgres

          meta = {
            description = "Bitcoin meta-protocol indexer (ordinals, BRC-20, runes)";
            mainProgram = "bitcoin-indexer";
          };
        };

        # ── Ordinals API (Node.js) ──
        ordinals-api = pkgs.buildNpmPackage {
          pname = "ordinals-api";
          version = "1.0.0";
          src = ./api/ordinals;
          npmDepsHash = ""; # TODO: populate after first build
          buildPhase = "npm run build";
          installPhase = ''
            mkdir -p $out/lib/ordinals-api
            cp -r dist node_modules package.json $out/lib/ordinals-api/
          '';
          meta.description = "Ordinals REST API for bitcoin-indexer";
        };

        # ── Runes API (Node.js) ──
        runes-api = pkgs.buildNpmPackage {
          pname = "runes-api";
          version = "1.0.0";
          src = ./api/runes;
          npmDepsHash = ""; # TODO: populate after first build
          buildPhase = "npm run build";
          installPhase = ''
            mkdir -p $out/lib/runes-api
            cp -r dist node_modules package.json $out/lib/runes-api/
          '';
          meta.description = "Runes REST API for bitcoin-indexer";
        };
      in
      {
        packages = {
          default = bitcoin-indexer;
          inherit bitcoin-indexer ordinals-api runes-api;
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            rustc cargo rust-analyzer clippy rustfmt
            pkg-config openssl snappy zlib bzip2 lz4 zstd libunwind
            llvmPackages_18.clang llvmPackages_18.llvm
            nodejs_20 postgresql_17
          ];
          LIBCLANG_PATH = "${pkgs.llvmPackages_18.libclang.lib}/lib";
        };
      }
    ) // {
      # ══════════════════════════════════════════════════════
      # NixOS Module — declarative configuration for bitcoin-indexer
      # Generates config.toml from Nix options, manages systemd services.
      # ══════════════════════════════════════════════════════
      nixosModules.default = { config, lib, pkgs, ... }:
        let
          inherit (lib) mkEnableOption mkOption mkIf types optional
            optionalString optionalAttrs;
          cfg = config.services.bitcoin-indexer;

          # Helper: generate a [db] TOML section from Nix attrs
          mkDbToml = name: db: ''
            [${name}.db]
            database = "${db.database}"
            host = "${db.host}"
            port = ${toString db.port}
            username = "${db.user}"
            ${optionalString (db.password != "") "password = \"${db.password}\""}
          '';

          # Generate the full config.toml from Nix options
          configToml = ''
            [storage]
            working_dir = "${cfg.dataDir}"

            [bitcoind]
            network = "${cfg.network}"
            rpc_url = "${cfg.bitcoind.rpcUrl}"
            rpc_username = "${cfg.bitcoind.rpcUser}"
            rpc_password = "${cfg.bitcoind.rpcPassword}"
            zmq_url = "${cfg.bitcoind.zmqUrl}"

            [resources]
            ulimit = ${toString cfg.resources.ulimit}
            cpu_core_available = ${toString cfg.resources.cpuCores}
            memory_available = ${toString cfg.resources.memoryGb}
            bitcoind_rpc_threads = ${toString cfg.resources.rpcThreads}
            bitcoind_rpc_timeout = ${toString cfg.resources.rpcTimeout}

            ${optionalString cfg.metrics.enable ''
            [metrics]
            enabled = true
            prometheus_port = ${toString cfg.metrics.port}
            ''}

            ${optionalString cfg.ordinals.enable ''
            ${mkDbToml "ordinals" cfg.ordinals.db}

            ${optionalString cfg.ordinals.brc20.enable ''
            [ordinals.meta_protocols.brc20]
            enabled = true
            lru_cache_size = ${toString cfg.ordinals.brc20.lruCacheSize}

            ${mkDbToml "ordinals.meta_protocols.brc20" cfg.ordinals.brc20.db}
            ''}
            ''}

            ${optionalString cfg.runes.enable ''
            [runes]
            lru_cache_size = ${toString cfg.runes.lruCacheSize}

            ${mkDbToml "runes" cfg.runes.db}
            ''}
          '';

          # PG database config sub-module
          pgDbType = types.submodule {
            options = {
              database = mkOption { type = types.str; };
              host = mkOption { type = types.str; default = "127.0.0.1"; };
              port = mkOption { type = types.port; default = 5432; };
              user = mkOption { type = types.str; default = "postgres"; };
              password = mkOption { type = types.str; default = ""; };
            };
          };
        in {
          options.services.bitcoin-indexer = {
            enable = mkEnableOption "Bitcoin Indexer (ordinals, BRC-20, runes)";

            package = mkOption {
              type = types.package;
              default = self.packages.${pkgs.system}.bitcoin-indexer;
              description = "bitcoin-indexer package to use.";
            };

            network = mkOption {
              type = types.enum [ "mainnet" "testnet" "signet" "regtest" ];
              default = "mainnet";
            };

            dataDir = mkOption {
              type = types.str;
              default = "/data/bitcoin-indexer";
            };

            configFile = mkOption {
              type = types.nullOr types.path;
              default = null;
              description = "Override: provide your own config.toml instead of generating from Nix options.";
            };

            # ── bitcoind connection ──
            bitcoind = {
              rpcUrl = mkOption {
                type = types.str;
                description = "Bitcoin Core RPC URL.";
              };
              rpcUser = mkOption { type = types.str; };
              rpcPassword = mkOption { type = types.str; };
              zmqUrl = mkOption {
                type = types.str;
                description = "ZMQ block notification URL.";
              };
            };

            # ── ordinals indexing ──
            ordinals = {
              enable = mkOption { type = types.bool; default = true; };
              db = mkOption { type = pgDbType; };

              brc20 = {
                enable = mkOption { type = types.bool; default = true; };
                lruCacheSize = mkOption { type = types.int; default = 50000; };
                db = mkOption { type = pgDbType; };
              };
            };

            # ── runes indexing ──
            runes = {
              enable = mkOption { type = types.bool; default = true; };
              lruCacheSize = mkOption { type = types.int; default = 50000; };
              db = mkOption { type = pgDbType; };
            };

            # ── resource limits ──
            resources = {
              ulimit = mkOption { type = types.int; default = 4096; };
              cpuCores = mkOption { type = types.int; default = 4; };
              memoryGb = mkOption { type = types.int; default = 8; };
              rpcThreads = mkOption { type = types.int; default = 4; };
              rpcTimeout = mkOption { type = types.int; default = 15; };
            };

            # ── prometheus metrics ──
            metrics = {
              enable = mkOption { type = types.bool; default = true; };
              port = mkOption { type = types.port; default = 9200; };
            };

            # ── ordinals API (Node.js) ──
            ordinalsApi = {
              enable = mkOption { type = types.bool; default = false; };
              port = mkOption { type = types.port; default = 3003; };
            };

            # ── runes API (Node.js) ──
            runesApi = {
              enable = mkOption { type = types.bool; default = false; };
              port = mkOption { type = types.port; default = 3001; };
            };
          };

          config = mkIf cfg.enable {
            # Write generated config.toml
            environment.etc."bitcoin-indexer/config.toml".text =
              if cfg.configFile != null
              then builtins.readFile cfg.configFile
              else configToml;

            # ── Main indexer service ──
            systemd.services.bitcoin-indexer = {
              description = "Bitcoin Indexer (${cfg.network})";
              after = [ "network-online.target" ];
              wants = [ "network-online.target" ];
              wantedBy = [ "multi-user.target" ];
              serviceConfig = {
                Type = "simple";
                User = "bitcoin-indexer";
                Group = "bitcoin-indexer";
                ExecStart = "${cfg.package}/bin/bitcoin-indexer ordinals service start --config-path /etc/bitcoin-indexer/config.toml";
                Restart = "on-failure";
                RestartSec = 30;
                LimitNOFILE = cfg.resources.ulimit;
                StateDirectory = "bitcoin-indexer";
              };
            };

            # ── Runes indexer (separate process if runes enabled) ──
            systemd.services.bitcoin-indexer-runes = mkIf cfg.runes.enable {
              description = "Bitcoin Indexer — Runes (${cfg.network})";
              after = [ "network-online.target" ];
              wants = [ "network-online.target" ];
              wantedBy = [ "multi-user.target" ];
              serviceConfig = {
                Type = "simple";
                User = "bitcoin-indexer";
                Group = "bitcoin-indexer";
                ExecStart = "${cfg.package}/bin/bitcoin-indexer runes service start --config-path /etc/bitcoin-indexer/config.toml";
                Restart = "on-failure";
                RestartSec = 30;
                LimitNOFILE = cfg.resources.ulimit;
              };
            };

            # ── Ordinals API (Node.js, optional) ──
            systemd.services.ordinals-api = mkIf cfg.ordinalsApi.enable {
              description = "Ordinals REST API (${cfg.network})";
              after = [ "bitcoin-indexer.service" ];
              wants = [ "bitcoin-indexer.service" ];
              wantedBy = [ "multi-user.target" ];
              environment = {
                PGHOST = cfg.ordinals.db.host;
                PGPORT = toString cfg.ordinals.db.port;
                PGUSER = cfg.ordinals.db.user;
                PGDATABASE = cfg.ordinals.db.database;
                API_HOST = "0.0.0.0";
                API_PORT = toString cfg.ordinalsApi.port;
              } // optionalAttrs (cfg.ordinals.db.password != "") {
                PGPASSWORD = cfg.ordinals.db.password;
              };
              serviceConfig = {
                Type = "simple";
                DynamicUser = true;
                ExecStart = "${pkgs.nodejs_20}/bin/node ${self.packages.${pkgs.system}.ordinals-api}/lib/ordinals-api/dist/src/index.js";
                Restart = "on-failure";
                RestartSec = 5;
              };
            };

            # ── Runes API (Node.js, optional) ──
            systemd.services.runes-api = mkIf cfg.runesApi.enable {
              description = "Runes REST API (${cfg.network})";
              after = [ "bitcoin-indexer-runes.service" ];
              wants = [ "bitcoin-indexer-runes.service" ];
              wantedBy = [ "multi-user.target" ];
              environment = {
                PGHOST = cfg.runes.db.host;
                PGPORT = toString cfg.runes.db.port;
                PGUSER = cfg.runes.db.user;
                PGDATABASE = cfg.runes.db.database;
                API_HOST = "0.0.0.0";
                API_PORT = toString cfg.runesApi.port;
              } // optionalAttrs (cfg.runes.db.password != "") {
                PGPASSWORD = cfg.runes.db.password;
              };
              serviceConfig = {
                Type = "simple";
                DynamicUser = true;
                ExecStart = "${pkgs.nodejs_20}/bin/node ${self.packages.${pkgs.system}.runes-api}/lib/runes-api/dist/src/index.js";
                Restart = "on-failure";
                RestartSec = 5;
              };
            };

            users.users.bitcoin-indexer = {
              isSystemUser = true;
              group = "bitcoin-indexer";
              home = cfg.dataDir;
            };
            users.groups.bitcoin-indexer = {};

            systemd.tmpfiles.rules = [
              "d ${cfg.dataDir} 0750 bitcoin-indexer bitcoin-indexer -"
            ];

            networking.firewall.allowedTCPPorts =
              optional cfg.metrics.enable cfg.metrics.port
              ++ optional cfg.ordinalsApi.enable cfg.ordinalsApi.port
              ++ optional cfg.runesApi.enable cfg.runesApi.port;
          };
        };
    };
}
