AIKE := ../aike/target/debug/aike

build:
	mbx build
	cp target/debug/browser.dll tests/browser.aike

test: build
	$(AIKE) -m tests/import_zapi.mjs
	$(AIKE) -m tests/regression_launch_race.mjs
	$(AIKE) -m tests/regression_launch_failure_cleanup.mjs
	$(AIKE) -m tests/regression_profile_dir_cleanup.mjs

clean:
	mbx clean
	rm -f tests/browser.aike

.PHONY: build test clean
