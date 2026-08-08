#![cfg(feature = "discord")]

use std::sync::Arc;

use anyhow::{Context as _, Result};
use serenity::all::{Client, Context, EventHandler, GatewayIntents, Message, Ready};
use serenity::async_trait;

use twlinter::core::{CoreEngine, CoreOptions};
use twlinter::discord_policy::{
    automatic_reply, rewrite_is_safe, rewrite_request, REWRITE_COMMAND,
};
use twlinter::gemini::GeminiClient;
use twlinter::llm::{validate_context_response, ContextRequest};

struct Handler {
    engine: Arc<CoreEngine>,
    gemini: Option<GeminiClient>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, message: Message) {
        if message.author.bot || message.content.trim().is_empty() {
            return;
        }

        let rewrite = message.content.strip_prefix(REWRITE_COMMAND);
        let source = rewrite.unwrap_or(&message.content);
        let analysis = self.engine.analyze(source);
        let context_request = ContextRequest::from_analysis(source, &analysis);
        let decisions = if let Some(gemini) = &self.gemini {
            if context_request.issues.is_empty() {
                Vec::new()
            } else {
                let gemini = gemini.clone();
                let request = context_request.clone();
                match tokio::task::spawn_blocking(move || gemini.choose_context(&request)).await {
                    Ok(Ok(response)) => {
                        match validate_context_response(&context_request, response) {
                            Ok(decisions) => decisions,
                            Err(error) => {
                                tracing::warn!(%error, "discarding invalid Gemini context decision");
                                Vec::new()
                            }
                        }
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "Gemini context decision failed");
                        Vec::new()
                    }
                    Err(error) => {
                        tracing::warn!(%error, "Gemini worker failed");
                        Vec::new()
                    }
                }
            }
        } else {
            Vec::new()
        };

        let result = match self.engine.apply(&analysis, &decisions) {
            Ok(result) => result,
            Err(error) => {
                tracing::warn!(%error, "conversion failed");
                return;
            }
        };

        let reply = if rewrite.is_some() {
            if let Some(gemini) = &self.gemini {
                let request = rewrite_request(source, &result.text, &analysis);
                let request_for_call = request.clone();
                let gemini = gemini.clone();
                match tokio::task::spawn_blocking(move || gemini.rewrite(&request_for_call)).await {
                    Ok(Ok(response)) if rewrite_is_safe(&request, &response.rewritten_text) => {
                        let rewritten_analysis = self.engine.analyze(&response.rewritten_text);
                        if rewritten_analysis.issues.is_empty() {
                            Some(format!("建議改寫：\n{}", response.rewritten_text))
                        } else {
                            Some("改寫結果未通過 zh-TW 規則檢查，已取消回覆。".to_string())
                        }
                    }
                    Ok(Ok(_)) => Some("改寫結果改動了受保護內容，已取消回覆。".to_string()),
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "Gemini rewrite failed");
                        Some("Gemini 暫時無法完成改寫。".to_string())
                    }
                    Err(error) => {
                        tracing::warn!(%error, "Gemini rewrite worker failed");
                        Some("Gemini 暫時無法完成改寫。".to_string())
                    }
                }
            } else {
                Some("尚未設定 GEMINI_API_KEY，無法進行語境改寫。".to_string())
            }
        } else {
            automatic_reply(&result)
        };

        if let Some(reply) = reply {
            if let Err(error) = message.channel_id.say(&ctx.http, reply).await {
                tracing::warn!(%error, "failed to send Discord reply");
            }
        }
    }

    async fn ready(&self, _: Context, ready: Ready) {
        tracing::info!(user = %ready.user.name, "Discord bot connected");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    twlinter::trace::init("info");
    let discord_token = std::env::var("DISCORD_TOKEN").context("DISCORD_TOKEN is required")?;
    let gemini = std::env::var("GEMINI_API_KEY").ok().map(|key| {
        GeminiClient::new(
            key,
            std::env::var("GEMINI_MODEL").unwrap_or_else(|_| "gemini-2.5-flash".to_string()),
        )
    });
    let engine = Arc::new(CoreEngine::from_embedded(CoreOptions::default())?);
    let intents =
        GatewayIntents::GUILDS | GatewayIntents::GUILD_MESSAGES | GatewayIntents::MESSAGE_CONTENT;
    let handler = Handler { engine, gemini };
    let mut client = Client::builder(&discord_token, intents)
        .event_handler(handler)
        .await
        .context("failed to build Discord client")?;
    client.start().await.context("Discord client stopped")?;
    Ok(())
}
