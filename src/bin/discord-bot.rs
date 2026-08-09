#![cfg(feature = "discord")]

use std::sync::Arc;

use anyhow::{Context as _, Result};
use serenity::all::{
    Client, CommandDataOption, CommandDataOptionValue, CommandInteraction, CommandOptionType,
    Context, CreateAllowedMentions, CreateCommand, CreateCommandOption, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, CreateWebhook, EventHandler, ExecuteWebhook,
    GatewayIntents, Interaction, Member, Message, MessageType, Permissions, Ready,
};
use serenity::async_trait;

use twlinter::core::{CoreAnalysis, CoreEngine, CoreOptions};
use twlinter::discord_config::{
    set_feature, spelling_rule, ChannelConfig, DiscordConfig, DiscordLinter, ServerConfig,
};
use twlinter::discord_policy::{
    automatic_replacement, automatic_reply, rewrite_is_safe, rewrite_replacement, rewrite_reply,
    rewrite_request,
};
use twlinter::engine::disambig::DisambigStats;
use twlinter::gemini::GeminiClient;
use twlinter::llm::{validate_context_response, ContextRequest};
use twlinter::rules::ruleset::CaseRule;

const COMMAND_NAME: &str = "twlinter";

fn is_lintable_message(author_is_bot: bool, content: &str, kind: MessageType) -> bool {
    !author_is_bot
        && !content.trim().is_empty()
        && matches!(kind, MessageType::Regular | MessageType::InlineReply)
}

struct Handler {
    engine: Arc<CoreEngine>,
    gemini: Option<GeminiClient>,
    config: Arc<DiscordConfig>,
    linter: Arc<DiscordLinter>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, message: Message) {
        if !is_lintable_message(message.author.bot, &message.content, message.kind) {
            return;
        }

        let Some(guild_id) = message.guild_id else {
            return;
        };
        let server = self.config.server(guild_id.get());
        let channel = self
            .config
            .channel(guild_id.get(), message.channel_id.get());
        let Some(lint) = self.linter.lint(&message.content, &server, channel) else {
            return;
        };
        if lint.output.issues.is_empty() && !lint.input_was_simplified {
            return;
        }

        let analysis = CoreAnalysis {
            normalized_text: lint.normalized_text,
            input_was_simplified: lint.input_was_simplified,
            issues: lint.output.issues,
            disambiguation: DisambigStats::default(),
        };
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

        let replacement = if result.changed {
            self.contextual_replacement(
                &message.content,
                &analysis,
                &result,
                &server,
                channel,
                decisions.iter().all(|decision| decision.selected.is_some()),
            )
            .await
        } else {
            None
        };

        if let Some(replacement) = replacement {
            if webhook_identity(&message).is_some()
                && self.replace_message(&ctx, &message, &replacement).await
            {
                return;
            }

            self.send_reply(&ctx, &message, rewrite_reply(&replacement))
                .await;
        } else if let Some(reply) = automatic_reply(&result) {
            self.send_reply(&ctx, &message, reply).await;
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

    async fn contextual_replacement(
        &self,
        source: &str,
        analysis: &twlinter::core::CoreAnalysis,
        result: &twlinter::core::CoreResult,
        server: &ServerConfig,
        channel: ChannelConfig,
        allow_rewrite: bool,
    ) -> Option<String> {
        if !allow_rewrite {
            return automatic_replacement(result);
        }
        let Some(gemini) = &self.gemini else {
            return automatic_replacement(result);
        };

        let request = rewrite_request(source, &result.text, analysis);
        let request_for_call = request.clone();
        let gemini = gemini.clone();
        match tokio::task::spawn_blocking(move || gemini.rewrite(&request_for_call)).await {
            Ok(Ok(response))
                if !response.rewritten_text.trim().is_empty()
                    && rewrite_is_safe(&request, &response.rewritten_text)
                    && self
                        .linter
                        .lint(&response.rewritten_text, server, channel)
                        .is_some_and(|lint| lint.output.issues.is_empty()) =>
            {
                rewrite_replacement(&response.rewritten_text)
            }
            Ok(Ok(_)) => {
                tracing::warn!("discarding unsafe or invalid Gemini rewrite");
                automatic_replacement(result)
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, "Gemini rewrite failed; using deterministic replacement");
                automatic_replacement(result)
            }
            Err(error) => {
                tracing::warn!(%error, "Gemini rewrite worker failed; using deterministic replacement");
                automatic_replacement(result)
            }
        }
    }

    async fn replace_message(&self, ctx: &Context, source: &Message, content: &str) -> bool {
        let Some((username, avatar_url)) = webhook_identity(source) else {
            return false;
        };

        // ponytail: create/delete per replacement keeps webhook tokens transient; cache per
        // channel only if message volume makes webhook rate limits matter.
        let webhook = match source
            .channel_id
            .create_webhook(&ctx.http, CreateWebhook::new("TWLinter rewrite"))
            .await
        {
            Ok(webhook) => webhook,
            Err(error) => {
                tracing::warn!(%error, "failed to create rewrite webhook");
                return false;
            }
        };
        let sent = match webhook
            .execute(
                &ctx.http,
                true,
                ExecuteWebhook::new()
                    .content(content)
                    .username(username)
                    .avatar_url(avatar_url)
                    .allowed_mentions(CreateAllowedMentions::new()),
            )
            .await
        {
            Ok(Some(sent)) => sent,
            Ok(None) => {
                tracing::warn!("rewrite webhook returned no message");
                let _ = webhook.delete(&ctx.http).await;
                return false;
            }
            Err(error) => {
                tracing::warn!(%error, "failed to send rewrite webhook");
                let _ = webhook.delete(&ctx.http).await;
                return false;
            }
        };

        if let Err(error) = source.delete(&ctx.http).await {
            tracing::warn!(%error, "failed to delete source message after webhook send");
            if let Err(cleanup_error) = webhook.delete_message(&ctx.http, None, sent.id).await {
                tracing::warn!(%cleanup_error, "failed to clean up replacement message");
            }
            let _ = webhook.delete(&ctx.http).await;
            return false;
        }

        if let Err(error) = webhook.delete(&ctx.http).await {
            tracing::warn!(%error, "failed to delete temporary rewrite webhook");
        }
        true
    }

    async fn send_reply(&self, ctx: &Context, message: &Message, reply: String) {
        let outbound = CreateMessage::new()
            .content(reply)
            .reference_message(message)
            .allowed_mentions(CreateAllowedMentions::new());
        if let Err(error) = message.channel_id.send_message(&ctx.http, outbound).await {
            tracing::warn!(%error, "failed to send Discord reply");
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

        let Some(guild_id) = command.guild_id else {
            respond_ephemeral(ctx, &command, "這個指令只能在伺服器中使用。").await;
            return;
        };
        let operation = command
            .data
            .options
            .first()
            .map(|option| option.name.as_str());
        let response = match operation {
            Some("enable") => match self.config.update_channel(
                guild_id.get(),
                command.channel_id.get(),
                ChannelConfig { tracking: true },
            ) {
                Ok(()) => "已啟用目前頻道的 TWLinter tracking。".to_string(),
                Err(error) => format!("啟用失敗，設定尚未保存：{error}"),
            },
            Some("disable") => match self.config.update_channel(
                guild_id.get(),
                command.channel_id.get(),
                ChannelConfig { tracking: false },
            ) {
                Ok(()) => "已停用目前頻道的 TWLinter tracking。".to_string(),
                Err(error) => format!("停用失敗，設定尚未保存：{error}"),
            },
            Some("status") => {
                let server = self.config.server(guild_id.get());
                let channels = self.config.tracked_channels(guild_id.get());
                let features = server.features;
                let feature_status = format!(
                    "terminology={}, spacing={}, case_dictionary={}, custom_rules={}",
                    features.terminology,
                    features.spacing,
                    features.case_dictionary,
                    features.custom_rules
                );
                if channels.is_empty() {
                    format!("Server 功能：{feature_status}；目前沒有 tracking 頻道。")
                } else {
                    let list = channels
                        .iter()
                        .map(|channel_id| format!("<#{channel_id}>"))
                        .collect::<Vec<_>>()
                        .join("、");
                    format!("Server 功能：{feature_status}；目前 tracking 頻道：{list}")
                }
            }
            Some("feature") => {
                let name = required_string(&command, "feature");
                let enabled = required_bool(&command, "enabled");
                match (name, enabled) {
                    (Ok(name), Ok(enabled)) => {
                        let mut server = self.config.server(guild_id.get());
                        match set_feature(&mut server.features, name, enabled)
                            .and_then(|()| self.config.update_server(guild_id.get(), server))
                        {
                            Ok(()) => format!("已將 server 功能 `{name}` 設為 `{enabled}`。"),
                            Err(error) => format!("設定失敗，設定尚未保存：{error}"),
                        }
                    }
                    (Err(error), _) | (_, Err(error)) => error,
                }
            }
            Some("rule") => {
                let from = required_string(&command, "from");
                let to = required_string(&command, "to");
                match (from, to) {
                    (Ok(from), Ok(to)) => {
                        let mut server = self.config.server(guild_id.get());
                        server.custom_spelling_rules.push(spelling_rule(from, to));
                        match self.config.update_server(guild_id.get(), server) {
                            Ok(()) => format!("已新增 server 用語規則：`{from}` → `{to}`。"),
                            Err(error) => format!("設定失敗，設定尚未保存：{error}"),
                        }
                    }
                    (Err(error), _) | (_, Err(error)) => error,
                }
            }
            Some("case") => match required_string(&command, "term") {
                Ok(term) => {
                    let mut server = self.config.server(guild_id.get());
                    server.custom_case_rules.push(serenity_case_rule(term));
                    match self.config.update_server(guild_id.get(), server) {
                        Ok(()) => format!("已新增 server 專有名詞大小寫規則：`{term}`。"),
                        Err(error) => format!("設定失敗，設定尚未保存：{error}"),
                    }
                }
                Err(error) => error,
            },
            _ => "請選擇 enable、disable、status、feature、rule 或 case。".to_string(),
        };
        respond_ephemeral(ctx, &command, response).await;
    }
}

fn webhook_identity(message: &Message) -> Option<(String, String)> {
    if message.guild_id.is_none()
        || message.member.is_none()
        || !message.attachments.is_empty()
        || !message.embeds.is_empty()
        || message.message_reference.is_some()
        || message.poll.is_some()
        || !message.sticker_items.is_empty()
    {
        return None;
    }

    let partial_member = message.member.as_ref()?;
    let username = partial_member
        .nick
        .as_deref()
        .or(message.author.global_name.as_deref())
        .unwrap_or(&message.author.name)
        .to_string();
    let mut partial_member = (**partial_member).clone();
    partial_member.user = Some(message.author.clone());
    partial_member.guild_id = message.guild_id;
    let member: Member = partial_member.into();
    Some((username, member.face()))
}

fn configuration_command() -> CreateCommand {
    let feature = CreateCommandOption::new(
        CommandOptionType::SubCommand,
        "feature",
        "設定 server-level linter 功能",
    )
    .add_sub_option(
        CreateCommandOption::new(CommandOptionType::String, "feature", "功能名稱")
            .required(true)
            .add_string_choice("terminology", "terminology")
            .add_string_choice("spacing", "spacing")
            .add_string_choice("case_dictionary", "case_dictionary")
            .add_string_choice("custom_rules", "custom_rules"),
    )
    .add_sub_option(
        CreateCommandOption::new(CommandOptionType::Boolean, "enabled", "是否開啟").required(true),
    );
    let rule = CreateCommandOption::new(
        CommandOptionType::SubCommand,
        "rule",
        "新增 server-level 用語規則",
    )
    .add_sub_option(
        CreateCommandOption::new(CommandOptionType::String, "from", "要被替換的詞").required(true),
    )
    .add_sub_option(
        CreateCommandOption::new(CommandOptionType::String, "to", "建議詞").required(true),
    );
    let case = CreateCommandOption::new(
        CommandOptionType::SubCommand,
        "case",
        "新增 server-level 專有名詞大小寫規則",
    )
    .add_sub_option(
        CreateCommandOption::new(CommandOptionType::String, "term", "正確大小寫").required(true),
    );

    CreateCommand::new(COMMAND_NAME)
        .description("設定 TWLinter server 功能與 tracking 頻道")
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
        .add_option(feature)
        .add_option(rule)
        .add_option(case)
}

fn required_string<'a>(command: &'a CommandInteraction, name: &str) -> Result<&'a str, String> {
    subcommand_option(command, name)
        .and_then(|option| option.value.as_str())
        .ok_or_else(|| format!("缺少參數 `{name}`。"))
}

fn required_bool(command: &CommandInteraction, name: &str) -> Result<bool, String> {
    subcommand_option(command, name)
        .and_then(|option| option.value.as_bool())
        .ok_or_else(|| format!("缺少參數 `{name}`。"))
}

fn subcommand_option<'a>(
    command: &'a CommandInteraction,
    name: &str,
) -> Option<&'a CommandDataOption> {
    let subcommand = command.data.options.first()?;
    let CommandDataOptionValue::SubCommand(options) = &subcommand.value else {
        return None;
    };
    options.iter().find(|option| option.name == name)
}

fn serenity_case_rule(term: &str) -> CaseRule {
    CaseRule {
        term: term.to_owned(),
        alternatives: None,
        disabled: false,
    }
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
    let config = Arc::new(DiscordConfig::from_env()?);
    let linter = Arc::new(DiscordLinter::new()?);
    let gemini = std::env::var("GEMINI_API_KEY").ok().map(|key| {
        GeminiClient::new(
            key,
            std::env::var("GEMINI_MODEL").unwrap_or_else(|_| "gemini-3.5-flash-lite".to_string()),
        )
    });
    let engine = Arc::new(CoreEngine::from_embedded(CoreOptions::default())?);
    let intents =
        GatewayIntents::GUILDS | GatewayIntents::GUILD_MESSAGES | GatewayIntents::MESSAGE_CONTENT;
    let handler = Handler {
        engine,
        gemini,
        config,
        linter,
    };
    let mut client = Client::builder(&discord_token, intents)
        .event_handler(handler)
        .await
        .context("failed to build Discord client")?;
    client.start().await.context("Discord client stopped")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_lintable_message;
    use serenity::all::MessageType;

    #[test]
    fn replies_are_lintable() {
        assert!(is_lintable_message(
            false,
            "需要糾錯",
            MessageType::InlineReply
        ));
    }
}
