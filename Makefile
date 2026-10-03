AIKE := ../aike/target/debug/aike

# Rust names the cdylib differently per platform.
ifeq ($(OS),Windows_NT)
	LIB := target/debug/browser.dll
else ifeq ($(shell uname -s),Darwin)
	LIB := target/debug/libbrowser.dylib
else
	LIB := target/debug/libbrowser.so
endif

build:
	mbx build
	cp $(LIB) tests/browser.aike

test: build
	$(AIKE) -m tests/import_zapi.mjs
	$(AIKE) -m tests/regression_launch_race.mjs
	$(AIKE) -m tests/regression_launch_failure_cleanup.mjs
	$(AIKE) -m tests/regression_profile_dir_cleanup.mjs
	$(AIKE) -m tests/regression_p0_fetch.mjs
	$(AIKE) -m tests/regression_p0_input.mjs
	$(AIKE) -m tests/regression_p0_runtime.mjs
	$(AIKE) -m tests/regression_p0_network.mjs
	$(AIKE) -m tests/regression_p0_window.mjs
	$(AIKE) -m tests/regression_p0_event_exit.mjs

clean:
	mbx clean
	rm -f tests/browser.aike

.PHONY: build test clean
