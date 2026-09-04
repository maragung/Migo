# Migo — top-level developer entrypoints.
# Everything a newcomer needs should be reachable from `make help`.

SHELL      := /bin/bash
.SHELLFLAGS := -eu -o pipefail -c
.DEFAULT_GOAL := help

SERVER_DIR  := server
DESKTOP_DIR := clients/desktop
WEB_DIR     := clients/web
ANDROID_DIR := clients/android
CARGO       := cargo
PNPM        := pnpm

# Cargo takes --manifest-path *after* the subcommand, not before it, so the flag
# cannot live in $(CARGO). Every recipe below places it explicitly; a target that
# forgets it silently operates on whatever workspace the caller happens to be in.
MANIFEST   := --manifest-path $(SERVER_DIR)/Cargo.toml

# The desktop client is a *separate* workspace, not a member of the server one, so it needs
# its own --manifest-path everywhere. That separation is deliberate and worth keeping in
# mind when editing these recipes: eframe drags in winit, glutin and a windowing stack that
# the server has no use for, and a shared workspace would put all of it in the server's
# Cargo.lock and in every server build's dependency graph. The cost is the duplication
# below; the benefit is that `make check` on a headless box still only builds a server.
DESKTOP_MANIFEST := --manifest-path $(DESKTOP_DIR)/Cargo.toml

# Colours only when attached to a TTY.
ifneq ($(shell test -t 1 && echo tty),)
  C_H := \033[1;36m
  C_R := \033[0m
else
  C_H :=
  C_R :=
endif

.PHONY: help
help: ## Show this help
	@printf "$(C_H)Migo — make targets$(C_R)\n\n"
	@grep -hE '^[a-zA-Z0-9_-]+:.*?## ' $(MAKEFILE_LIST) \
		| sort \
		| awk 'BEGIN{FS=":.*?## "}{printf "  \033[1m%-18s\033[0m %s\n", $$1, $$2}'

# ---------------------------------------------------------------- setup

.PHONY: setup
setup: ## Install toolchain components and JS dependencies
	rustup component add rustfmt clippy 2>/dev/null || true
	$(PNPM) install

.PHONY: protocol
protocol: ## Regenerate Rust + TypeScript protocol code from shared/protocol/schema
	node tools/protocol-codegen/generate.mjs

.PHONY: protocol-check
protocol-check: ## Fail if generated protocol code is stale (CI gate)
	node tools/protocol-codegen/generate.mjs --check

.PHONY: entities
entities: ## Regenerate SeaORM entities from server/migrations
	node tools/entity-codegen/generate.mjs

.PHONY: entity-check
entity-check: ## Fail if generated entities are stale (CI gate)
	node tools/entity-codegen/generate.mjs --check

.PHONY: brief-check
brief-check: ## Fail if migo.md contradicts the schema or docs (CI gate, see brief section 178)
	python3 tools/scripts/brief-audit.py

.PHONY: vectors
vectors: ## Regenerate the cross-language conformance vectors in shared/protocol/vectors
	# The wire and crypto generators are independent implementations written from the
	# specification and the RFCs; the crypto one refuses to emit anything until it
	# reproduces the published test vectors it was written against. The account
	# generator follows the same policy for the HKDF domains and BIP-32/EIP-55;
	# the two rust-reference files (ML-DSA, .migo container) come from the
	# migo-account example binary, and that provenance is recorded in each file.
	python3 tools/vectors/generate_wire_vectors.py
	python3 tools/vectors/generate_crypto_vectors.py
	python3 tools/vectors/generate_account_vectors.py
	$(CARGO) run $(MANIFEST) -p migo-account --example write_reference_vectors

.PHONY: vector-check
vector-check: ## Fail if the committed vectors are stale (CI gate, no Rust toolchain needed)
	# Separate from the runners on purpose. If a generator now produces different
	# bytes, the interesting failure is "the vectors moved" — and this target
	# answers that in two seconds, without a Rust toolchain, so it can live in the
	# fast gate job alongside protocol-check. The two rust-reference account files
	# are not checked here (they need cargo); test-vectors-rust covers them.
	python3 tools/vectors/generate_wire_vectors.py --check
	python3 tools/vectors/generate_crypto_vectors.py --check --quiet
	python3 tools/vectors/generate_account_vectors.py --check --quiet

.PHONY: kotlin-check
kotlin-check: ## Static checks on the Android Kotlin, which nothing here can compile (CI gate)
	# The Android module needs a JDK, the Android SDK and Gradle, so android.yml is the
	# only place in this project that compiles Kotlin at all — every mistake in that tree
	# costs a push and a runner. This target checks the handful of properties that are
	# cheap to read off the text: block comments that never close (Kotlin's nest, so a
	# slash-star in prose swallows the rest of the file and reports at EOF), Cyrillic
	# homoglyphs, and imports that are unused, duplicated or unsorted. It is not a type
	# checker and cannot be one without the classpath; see the script's header.
	#
	# The checker runs against itself first. It is the only gate here with no compiler
	# standing behind it, so a regex that quietly stops matching would turn it into a
	# green light rather than a broken build, and that is the one failure mode a gate
	# must not have. --selftest breaks a copy of each shape on purpose and also checks
	# the shapes that must NOT be reported.
	python3 tools/scripts/kotlin-lint.py --selftest
	python3 tools/scripts/kotlin-lint.py $(ANDROID_DIR)

.PHONY: infra-check
infra-check: ## Static hygiene checks on infra/ that need no daemon (CI gate)
	# Deployment files are the one tree where a mistake is invisible until it is
	# already running somewhere: an unpinned tag that silently moves, a secret typed
	# into a compose file, a container that asked for the host's namespaces. None of
	# that needs Docker to see, so this gate reads the files instead of starting a
	# stack. It checks pinned images, private key material and secret-shaped values
	# outside the two documented development constants, privileged containers, host
	# namespaces and writable host mounts, requests, limits and both probes on every
	# Kubernetes workload, two services publishing the same host port, and the web
	# client publishing exactly port 19991.
	#
	# It starts no container, so it is not a substitute for a smoke test; brief
	# section 177 keeps infra out of BUILT for precisely that reason.
	python3 tools/scripts/infra-audit.py

.PHONY: pydeps-check
pydeps-check: ## The CI gate installs exactly the Python modules tools/ imports
	# Written after a red build. The gate job pins an interpreter so that pip install is
	# permitted at all, and pinning one also replaces whatever the runner image happened
	# to pre-install. The crypto vector generator imported cryptography, the image had
	# it, nothing declared it, and the gate broke the moment the interpreter moved.
	#
	# So the pip list in the workflow is a declaration, and this reads both sides of it:
	# the imports under tools/ and the install line in .github/workflows/ci.yml. A
	# module imported but not installed is that failure. A module installed but not
	# imported is the reverse, an install line that outlived its reason. Both fail here.
	python3 tools/scripts/pydeps-audit.py

.PHONY: secret-check
secret-check: ## Fail if a secret-shaped string is committed anywhere (CI gate, brief section 183)
	# The committed half of brief section 183: no credential format -- token,
	# key, private key block, signed JWT, literal password in a URL -- may
	# land in a tracked file. The runtime half (redaction before anything
	# leaves the process) is the loadgen redaction filter and the config
	# summary/Debug tests, which this scanner's allowlist documents rather
	# than duplicates. Well-known formats only: a generic hunt for
	# high-entropy "passwords" cannot tell a secret from a test vector, and
	# a gate that cries wolf is a gate everyone scrolls past.
	python3 tools/scripts/secret-audit.py

# ---------------------------------------------------------------- build

.PHONY: build
build: build-server build-ts build-web ## Build everything

.PHONY: build-server
build-server: ## Build the Rust workspace
	$(CARGO) build $(MANIFEST) --workspace

.PHONY: build-release
build-release: ## Build the Rust workspace in release mode
	$(CARGO) build $(MANIFEST) --workspace --release

.PHONY: build-ts
build-ts: ## Build the TypeScript packages (protocol, wire, crypto, sdk)
	# One `tsc --build` at the root rather than one per package: the packages are
	# composite projects with references, so the root build works out the order itself
	# and skips what is already current. Building them one at a time would recompile
	# each shared dependency once per dependent.
	$(PNPM) run --if-present build

.PHONY: build-web
build-web: ## Build the Next.js web client
	$(PNPM) --filter @migo/web build

.PHONY: build-store
build-store: ## Build the React+Vite store app into the web export's /store
	# The store's Vite outDir is clients/web/out/store, so it lands inside the web bundle the
	# release tarball already carries and the :19992 file server already serves — /store/ is
	# the same bytes, same origin, sharing the web client's IndexedDB session.
	$(PNPM) --filter @migo/store build

.PHONY: contracts-check
contracts-check: ## The Solidity gates (forge fmt/build/test), when forge is installed
	# Local and convenience only: CI installs Foundry itself (contracts.yml), and a machine
	# without forge skips rather than fails — the workflow is the gate of record.
	@if command -v forge >/dev/null 2>&1; then \
		cd contracts && forge fmt --check && forge build && forge test; \
	else \
		echo "forge not found; the contracts workflow is the gate of record"; \
	fi

# ---------------------------------------------------------------- run

.PHONY: dev
dev: ## Run migod (in-memory store) + web client together
	tools/scripts/dev.sh

.PHONY: dev-server
dev-server: ## Run migod only, in-memory store, all roles
	MIGO_STORE__BACKEND=memory MIGO_CACHE__BACKEND=memory \
	  $(CARGO) run $(MANIFEST) -p migod -- serve

.PHONY: dev-pg
dev-pg: ## Run migod against Postgres + Redis from infra/compose
	MIGO_STORE__BACKEND=postgres MIGO_CACHE__BACKEND=redis \
	  $(CARGO) run $(MANIFEST) -p migod -- serve

.PHONY: dev-web
dev-web: ## Run the Next.js dev server only
	$(PNPM) --filter @migo/web dev

.PHONY: infra-up
infra-up: ## Start Postgres, Redis and MinIO via Docker Compose
	docker compose -f infra/compose/docker-compose.yml up -d

.PHONY: infra-down
infra-down: ## Stop the local infrastructure
	docker compose -f infra/compose/docker-compose.yml down

.PHONY: migrate
migrate: ## Apply SQL migrations to $$MIGO_STORE__URL
	$(CARGO) run $(MANIFEST) -p migod -- migrate

# ---------------------------------------------------------------- quality

.PHONY: check
check: check-server check-desktop ## Fast type-check of both Rust workspaces

.PHONY: check-server
check-server: ## Fast type-check of the server workspace
	$(CARGO) check $(MANIFEST) --workspace --all-targets

.PHONY: check-desktop
check-desktop: ## Fast type-check of the desktop client workspace
	# Checks, never builds. `cargo check` needs no OpenGL, X11 or Wayland development
	# headers because nothing is linked; winit and glutin open those libraries at run
	# time. That is what lets this target pass on a headless container, and it is also
	# why the release build of this crate is a CI job rather than something a developer
	# is expected to be able to link locally.
	$(CARGO) check $(DESKTOP_MANIFEST) --workspace --all-targets

.PHONY: fmt
fmt: ## Format Rust and TypeScript
	$(CARGO) fmt $(MANIFEST) --all
	$(CARGO) fmt $(DESKTOP_MANIFEST) --all
	# `run`, not `-r run`: pnpm's recursive mode deliberately excludes the workspace
	# root, and the root is where Prettier is configured. `-r` here ran a script that
	# no package defines, reported success, and formatted nothing.
	$(PNPM) run --if-present format

.PHONY: fmt-check
fmt-check: fmt-check-rust fmt-check-desktop fmt-check-js ## Verify formatting (CI gate)

# The -rust / -js split repeats through fmt-check, lint and test-vectors, and exists for
# CI rather than for people: the job with a warm target/ has no pnpm, the job with pnpm
# has no Rust toolchain, and a job that had to install both to run one gate would
# install both to run neither well. Run the unsuffixed target locally; it does both.
.PHONY: fmt-check-rust
fmt-check-rust: ## rustfmt only, server workspace, check mode (CI gate)
	$(CARGO) fmt $(MANIFEST) --all -- --check

.PHONY: fmt-check-desktop
fmt-check-desktop: ## rustfmt only, desktop workspace, check mode (CI gate)
	# Split out for the same reason as the -rust / -js pair: it is the gate for a
	# different CI job, one whose cache holds a windowing stack instead of a database
	# driver. clients/desktop/rustfmt.toml repeats the server's settings verbatim,
	# because rustfmt stops looking for configuration at the workspace root and would
	# otherwise format this half of the repository to its own defaults.
	$(CARGO) fmt $(DESKTOP_MANIFEST) --all -- --check

.PHONY: fmt-check-js
fmt-check-js: ## Prettier only, check mode (CI gate)
	# Prettier's scope is the whole tree, docs and workflows included, because
	# .prettierignore decides what is out of scope and not this recipe — whether a file
	# is formatted should be settled by one rule wherever the file lives.
	$(PNPM) run --if-present format:check

.PHONY: lint
lint: lint-rust lint-desktop lint-js ## Clippy (deny warnings) + ESLint

.PHONY: lint-rust
lint-rust: ## Clippy only, server workspace, warnings denied (CI gate)
	$(CARGO) clippy $(MANIFEST) --workspace --all-targets --all-features -- -D warnings

.PHONY: lint-desktop
lint-desktop: ## Clippy only, desktop workspace, warnings denied (CI gate)
	# --all-features is omitted on purpose. This crate's only features are eframe's
	# renderer backends, which are mutually exclusive in practice: asking for all of
	# them at once lints a configuration nobody ships. The default set is what the
	# release job builds, so it is what gets linted.
	$(CARGO) clippy $(DESKTOP_MANIFEST) --workspace --all-targets -- -D warnings

.PHONY: lint-js
lint-js: ## ESLint only, over the whole workspace (CI gate)
	# From the root, for the same reason there is one flat config: a rule enforced in
	# only some packages is a rule a reviewer cannot rely on. The typed rules need type
	# information, so this wants the build first — `make ci` orders it that way.
	$(PNPM) exec eslint .

.PHONY: doc-check
doc-check: ## Fail on broken intra-doc links (CI gate)
	# `cargo test` compiles doc examples but never resolves doc *links*, so a link to
	# a renamed or private item is a hole in the published documentation that every
	# other gate reports as green. RUSTDOCFLAGS rather than a global RUSTFLAGS: the
	# latter would also apply to dependencies, where somebody else's deprecation
	# warning would fail our build.
	RUSTDOCFLAGS="-D warnings" $(CARGO) doc $(MANIFEST) --workspace --no-deps

.PHONY: test
test: test-server test-web ## Run all tests

.PHONY: test-server
test-server: ## Run the Rust test suite
	$(CARGO) test $(MANIFEST) --workspace

.PHONY: test-contract
test-contract: ## Contract suites against real backends (needs MIGO_TEST_DATABASE_URL, MIGO_TEST_REDIS_URL)
	@test -n "$${MIGO_TEST_DATABASE_URL:-}" \
	  || echo "note: MIGO_TEST_DATABASE_URL unset — the Postgres half will pass by doing nothing"
	@test -n "$${MIGO_TEST_REDIS_URL:-}" \
	  || echo "note: MIGO_TEST_REDIS_URL unset — the Redis half will pass by doing nothing"
	$(CARGO) test $(MANIFEST) -p migo-store -p migo-cache

.PHONY: test-vectors
test-vectors: test-vectors-rust test-vectors-ts ## Cross-language conformance: Rust and TS must agree on the wire + crypto vectors

.PHONY: test-vectors-rust
test-vectors-rust: vector-check ## The Rust half of the conformance vectors (CI gate)
	# migo-account's consumer also covers the two rust-reference files: it
	# reseals and re-signs every case, which doubles as the staleness check
	# those files cannot get from the Python-only vector-check.
	$(CARGO) test $(MANIFEST) -p migo-wire -p migo-crypto -p migo-account --test vectors

.PHONY: test-vectors-ts
test-vectors-ts: vector-check ## The TypeScript half of the conformance vectors (CI gate)
	# Gated on the dependencies being installed rather than skipped silently, and the
	# gate fails: "0 packages" scrolling past in a green build is how a language binding
	# drifts for a month without anybody noticing, and this target's entire claim is
	# that both languages read the same bytes and agree.
	@if [ -d node_modules ]; then \
	  $(MAKE) --no-print-directory build-ts; \
	  $(PNPM) --filter @migo/protocol --filter @migo/wire --filter @migo/crypto test; \
	else \
	  echo "error: node_modules is missing, so the TypeScript half of the vector suite"; \
	  echo "       cannot run and this target must not claim success. Run 'make setup'."; \
	  exit 1; \
	fi

.PHONY: test-web
test-web: build-ts ## Run web/TypeScript tests
	# Depends on build-ts because each package's `test` script runs `node --test` over
	# dist/. With no build the glob matches nothing, node exits 0, and the suite passes
	# by not existing.
	$(PNPM) -r --if-present test

.PHONY: audit
audit: ## Dependency vulnerability + licence audit
	# cargo-audit reads the lockfile, not the manifest, so it takes --file rather
	# than --manifest-path. Both are `|| true` because an advisory published this
	# morning is news, not a reason to block an unrelated change.
	$(CARGO) audit --file $(SERVER_DIR)/Cargo.lock || true
	$(PNPM) audit --audit-level high || true

.PHONY: ci
ci: protocol-check entity-check brief-check vector-check kotlin-check infra-check pydeps-check secret-check fmt-check build-ts lint doc-check test test-vectors ## Everything CI runs

# ---------------------------------------------------------------- misc

.PHONY: clean
clean: ## Remove build artefacts
	$(CARGO) clean $(MANIFEST)
	$(CARGO) clean $(DESKTOP_MANIFEST)
	rm -rf $(WEB_DIR)/.next $(WEB_DIR)/out node_modules/.cache
