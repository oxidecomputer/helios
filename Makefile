
TOP =			$(PWD)

HELIOS_BUILD =		$(TOP)/tools/helios-build/target/debug/helios-build

.PHONY: welcome
welcome: gmakecheck
	@printf '\n'
	@printf 'Welcome to the Helios build system\n'
	@printf '\n'
	@printf '\n'
	@if ! cargo --version >/dev/null 2>&1; then \
		printf '    You must install Rust before continuing.\n'; \
	else \
		printf '    Try "gmake setup" to get started!\n'; \
	fi
	@printf '\n'

.PHONY: gmakecheck
gmakecheck:
	@if [[ -z "$(.FEATURES)" ]]; then \
		printf 'ERROR: This Makefile requires GNU Make (gmake)\n' >&2; \
		exit 1; \
	fi

#
# Run a "quick" build of illumos for development:
#
.PHONY: illumos
illumos: gmakecheck $(HELIOS_BUILD)
	$(HELIOS_BUILD) build-illumos -q

#
# Enter the "quick" build environment so that you can run dmake, etc:
#
.PHONY: bldenv
bldenv: gmakecheck $(HELIOS_BUILD)
	$(HELIOS_BUILD) bldenv -q

.PHONY: setup
setup: gmakecheck $(HELIOS_BUILD)
	@$(HELIOS_BUILD) setup
	rm -f helios-build
	ln -s tools/helios-build/target/debug/helios-build
	@printf '\n'
	@printf 'Setup complete!  ./helios-build is now available.\n'
	@printf '\n'

.PHONY: $(HELIOS_BUILD)
$(HELIOS_BUILD):
	@if [[ $$(/usr/bin/uname -o) != illumos ]]; then \
		printf 'ERROR: must be built on illumos\n' >&2; \
		exit 1; \
	fi
	cd tools/helios-build && cargo build --quiet

.PHONY: clean
clean:
	cd tools/helios-build && cargo clean --quiet
