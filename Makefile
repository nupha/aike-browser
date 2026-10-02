AIKE := ../aike/target/debug/aike

build:
	mbx build
	cp target/debug/browser.dll tests/browser.aike

test: build
	$(AIKE) -m tests/import_zapi.mjs

clean:
	mbx clean
	rm -f tests/browser.aike

.PHONY: build test clean
