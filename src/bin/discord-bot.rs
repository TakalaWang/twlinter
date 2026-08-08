#![cfg(feature = "discord")]

use std::sync::Arc;

use anyhow::{Context as _, Result};
use serenity::all::{
    Client, CommandInteraction, CommandOptionType, Context, CreateAllowedMentions, CreateCommand,
    CreateCommandOption, CreateInteractionResponse, CreateInteractionResponseMessage,
    CreateMessage, EventHandler, GatewayIntents, Interaction, Message, MessageType, Permissions,
    Ready,
};
use serenity::async_trait;

use twlinter::core::{CoreEngine, CoreOptions};
use twlinter::discord_channels::ChannelRegistry;
use twlinter::discord_policy::{automatic_reply, rewrite_is_safe, rewrite_reply, rewrite_request};
use twlinter::gemini::GeminiClient;
use twlinter::llm::{validate_context_response, ContextRequest};

const COMMAND_NAME: &str = "twlinter";

struct Handler {
    engine: Arc<CoreEngine>,
    gemini: Option<GeminiClient>,
    channels: Arc<ChannelRegistry>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, message: Message) {
        if message.author.bot
            || message.content.trim().is_empty()
            || message.kind != MessageType::Regular
            || !self.channels.is_enabled(message.channel_id.get())
        {
            return;
        }

        let analysis = self.engine.analyze(&message.content);
        let context_request = ContextRequest::from_analysis(&message.content, &analysis);
        let decisions = if let Some(gemini) = &self.gemini {
            if context_request.issues.is_empty() {
                Vec::new()
            } else {
                let gemini = gemini.clone();
                let request = context_request.clone();
                match tokio::task::spawn_blocking(move || gemini.choose_context(&request)).await {
                    Ok(Ok(response)) => match validate_context_response(&context_request, response)
                    {
                        Ok(decisions) => decisions,
                        Err(error) => {
                            tracing::warn!(%error, "discarding invalid Gemini context decision");
                            Vec::new()
                        }
                    },
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

        let reply = if result.changed {
            self.contextual_reply(&message.content, &analysis, &result)
                .await
        } else {
            None
        };

        if let Some(reply) = reply {
            let outbound = CreateMessage::new()
                .content(reply)
                .reference_message(&message)
                .allowed_mentions(CreateAllowedMentions::new());
            if let Err(error) = message.channel_id.send_message(&ctx.http, outbound).await {
                tracing::warn!(%error, "failed to send Discord reply");
            }
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let Interaction::Command(command) = interaction else {
            return;
        };
        if command.data.name == COMMAND_NAME {
            self.handle_configuration_command(&ctx, command).await;
        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        for guild in &ready.guilds {
            self.register_configuration_command(&ctx, guild.id).await;
        }
        tracing::info!(user = %ready.user.name, "Discord bot connected");
    }

    async fn guild_create(&self, ctx: Context, guild: serenity::all::Guild, _is_new: Option<bool>) {
        self.register_configuration_command(&ctx, guild.id).await;
    }
}

impl Handler {
    async fn register_configuration_command(
        &self,
        ctx: &Context,
        guild_id: serenity::all::GuildId,
    ) {
        if let Err(error) = guild_id
            .set_commands(&ctx.http, vec![configuration_command()])
            .await
        {
            tracing::warn!(%guild_id, %error, "failed to register configuration command");
        }
    }

    async fn contextual_reply(
        &self,
        source: &str,
        analysis: &twlinter::core::CoreAnalysis,
        result: &twlinter::core::CoreResult,
    ) -> Option<String> {
        let Some(gemini) = &self.gemini else {
            return automatic_reply(result);
        };

        let request = rewrite_request(source, &result.text, analysis);
        let request_for_call = request.clone();
        let gemini = gemini.clone();
        match tokio::task::spawn_blocking(move || gemini.rewrite(&request_for_call)).await {
            Ok(Ok(response))
                if !response.rewritten_text.trim().is_empty()
                    && rewrite_is_safe(&request, &response.rewritten_text)
                    && self
                        .engine
                        .analyze(&response.rewritten_text)
                        .issues
                        .is_empty() =>
            {
                Some(rewrite_reply(&response.rewritten_text))
            }
            Ok(Ok(_)) => {
                tracing::warn!("discarding unsafe or invalid Gemini rewrite");
                automatic_reply(result)
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, "Gemini rewrite failed; using deterministic reply");
                automatic_reply(result)
            }
            Err(error) => {
                tracing::warn!(%error, "Gemini rewrite worker failed; using deterministic reply");
                automatic_reply(result)
            }
        }
    }

    async fn handle_configuration_command(&self, ctx: &Context, command: CommandInteraction) {
        let can_manage = command
            .member
            .as_ref()
            .and_then(|member| member.permissions)
            .is_some_and(|permissions| {
                permissions.contains(Permissions::MANAGE_GUILD)
                    || permissions.contains(Permissions::ADMINISTRATOR)
            });
        if !can_manage {
            respond_ephemeral(
                ctx,
                &command,
                "只有具備管理伺服器權限的人可以設定 TWLinter。",
            )
            .await;
            return;
        }

        let operation = command
            .data
            .options
            .first()
            .map(|option| option.name.as_str());
        let response = match operation {
            Some("enable") => match self.channels.enable(command.channel_id.get()) {
                Ok(true) => "已啟用目前頻道的 TWLinter 自動情境改寫。".to_string(),
                Ok(false) => "目前頻道已經是啟用狀態。".to_string(),
                Err(error) => format!("啟用失敗，設定尚未保存：{error}"),
            },
            Some("disable") => match self.channels.disable(command.channel_id.get()) {
                Ok(true) => "已停用目前頻道的 TWLinter。".to_string(),
                Ok(false) => "目前頻道原本就沒有啟用。".to_string(),
                Err(error) => format!("停用失敗，設定尚未保存：{error}"),
            },
            Some("status") => {
                let channels = self.channels.list();
                if channels.is_empty() {
                    "目前沒有啟用任何頻道。請在要觸發的頻道使用 `/twlinter enable`。".to_string()
                } else {
                    let list = channels
                        .iter()
                        .map(|channel_id| format!("<#{channel_id}>"))
                        .collect::<Vec<_>>()
                        .join("、");
                    format!("目前啟用的頻道：{list}")
                }
            }
            _ => "請選擇 enable、disable 或 status。".to_string(),
        };
        respond_ephemeral(ctx, &command, response).await;
    }
}

fn configuration_command() -> CreateCommand {
    CreateCommand::new(COMMAND_NAME)
        .description("設定 TWLinter 自動改寫頻道")
        .default_member_permissions(Permissions::MANAGE_GUILD)
        .dm_permission(false)
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "enable",
            "啟用目前頻道的自動情境改寫",
        ))
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "disable",
            "停用目前頻道的自動情境改寫",
        ))
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "status",
            "查看已啟用的頻道",
        ))
}

async fn respond_ephemeral(
    ctx: &Context,
    command: &CommandInteraction,
    content: impl Into<String>,
) {
    let response = CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(content)
            .ephemeral(true),
    );
    if let Err(error) = command.create_response(&ctx.http, response).await {
        tracing::warn!(%error, "failed to respond to configuration command");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    twlinter::trace::init("info");
    let discord_token = std::env::var("DISCORD_TOKEN").context("DISCORD_TOKEN is required")?;
    let channels = Arc::new(ChannelRegistry::from_env()?);
    let gemini = std::env::var("GEMINI_API_KEY").ok().map(|key| {
        GeminiClient::new(
            key,
            std::env::var("GEMINI_MODEL").unwrap_or_else(|_| "gemini-2.5-flash".to_string()),
        )
    });
    let engine = Arc::new(CoreEngine::from_embedded(CoreOptions::default())?);
    let intents =
        GatewayIntents::GUILDS | GatewayIntents::GUILD_MESSAGES | GatewayIntents::MESSAGE_CONTENT;
    let handler = Handler {
        engine,
        gemini,
        channels,
    };
    let mut client = Client::builder(&discord_token, intents)
        .event_handler(handler)
        .await
        .context("failed to build Discord client")?;
    client.start().await.context("Discord client stopped")?;
    Ok(())
}
