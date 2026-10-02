AIKE := ../aike/target/debug/aike

build:
	mbx build
	cp target/debug/browser.dll tests/browser.aike

# Stand-in CDP server used by tests/regression_launch_failure_cleanup.mjs.
tests/fake_cdp_server.exe: tests/fake_cdp_server.rs
	rustc -O -o tests/fake_cdp_server.exe tests/fake_cdp_server.rs

test: build tests/fake_cdp_server.exe
	$(AIKE) -m tests/import_zapi.mjs
	$(AIKE) -m tests/regression_launch_race.mjs
	$(AIKE) -m tests/regression_launch_failure_cleanup.mjs
	$(AIKE) -m tests/regression_profile_dir_cleanup.mjs

clean:
	mbx clean
	rm -f tests/browser.aike tests/fake_cdp_server.exe tests/fake_cdp_server.pdb

.PHONY: build test clean
