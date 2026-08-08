## Summary

<!-- What changed and why? -->

## User impact

<!-- Describe CLI, core, extension, ruleset, or Discord behavior changes. -->

## Validation

- [ ] `cargo fmt --check`
- [ ] `cargo clippy --all-targets --features discord -- -D warnings`
- [ ] `cargo test --all-targets --features discord`
- [ ] `python3 scripts/check-ruleset.py --lint` (if rules or ruleset loading changed)
- [ ] `npm test --prefix extension` (if extension code changed)

## Upstream and attribution

<!-- If copied rules or engine behavior changed, explain the relationship to
     https://github.com/sysprog21/zhtw-mcp. -->

## Checklist

- [ ] No secrets, tokens, or private message content are included.
- [ ] Documentation and tests were updated where needed.
- [ ] Generated build output is not committed.
