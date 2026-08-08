# Discord bot

This adapter listens to Discord messages and delegates terminology detection,
Tier 1/Tier 2 rules, and post-fix validation to `zhtw_core::core::CoreEngine`.
Gemini is optional: it only selects from ruleset-provided candidates during
automatic replies and performs a full rewrite for the explicit `/tw-rewrite`
command.

## Run

1. Create a Discord application and bot, enable the Message Content Intent,
   and invite it with the `bot` scope and permission to read/send messages.
2. Copy `config.example.env`, fill in the tokens, and export the variables.
3. Start the bot from the repository root:

```bash
cargo run --release --features discord --bin zhtw-discord-bot
```

`GEMINI_API_KEY` may be omitted. Without it, deterministic and locally
resolved corrections still work; ambiguous terms remain unchanged.
