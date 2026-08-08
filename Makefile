S2T_DATA := src/engine/s2t_data.rs
OPENCC_DICT_DIR := data/opencc

all: $(S2T_DATA)
	cargo build --release

# gen-s2t-tables.py handles downloading from GitHub + code generation.
$(S2T_DATA): scripts/gen-s2t-tables.py
	python3 scripts/gen-s2t-tables.py
	rustfmt $@

clean:
	cargo clean

distclean: clean
	rm -f $(S2T_DATA)
	rm -rf $(OPENCC_DICT_DIR)

check: $(S2T_DATA)
	cargo test
	cargo clippy -- -D warnings
	cargo fmt --check
	python3 scripts/check-ruleset.py --lint

check-size: all
	@SIZE=$$(wc -c < target/release/zhtw-mcp | tr -d ' '); \
	MAX=20971520; \
	if [ "$$SIZE" -gt "$$MAX" ]; then \
		echo "FAIL: release binary $$SIZE bytes exceeds 20 MiB budget ($$MAX)"; \
		exit 1; \
	else \
		echo "OK: release binary $$SIZE bytes (budget: $$MAX)"; \
	fi

indent: $(S2T_DATA)
	cargo fmt
	python3 scripts/check-ruleset.py
	python3 scripts/check-ruleset.py --lint
	black scripts/*.py

corpus: $(S2T_DATA)
	cargo test --test corpus-evaluation -- --nocapture

.PHONY: all clean distclean check check-size corpus indent install uninstall status

install: all
	@./scripts/deploy.sh install

uninstall:
	@./scripts/deploy.sh uninstall

status:
	@./scripts/deploy.sh status
