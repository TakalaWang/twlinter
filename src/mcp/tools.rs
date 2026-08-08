// MCP tool handler implementations.
//
// One tool exposed to the MCP client:
//   zhtw — unified lint / fix / gate for Traditional Chinese (Taiwan) text

use std::cell::OnceCell;
use std::sync::Arc;

use serde::Serialize;
use serde_json::{json, Value};

use super::prompts;
use super::resources;
use super::sampling::{refine_issues_with_sampling, SamplingBridge, SamplingStats};
use super::telemetry::{TelemetryMetrics, TokenTelemetry};
use super::types::{
    CallToolParams, CallToolResult, ClientCapabilities, InitializeParams, InitializeResult,
    JsonRpcRequest, JsonRpcResponse, PromptCapability, PromptGetParams, ResourceCapability,
    ResourceReadParams, ServerCapabilities, ServerInfo, ToolAnnotations, ToolCapability, ToolDef,
    ToolsListResult, INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, MCP_PROTOCOL_VERSION,
    METHOD_NOT_FOUND, SERVER_NOT_INITIALIZED,
};
use crate::audit::Trace;
use crate::engine::disambig::{disambiguate_batch, DisambigConfig, DisambigStats};
use crate::engine::excluded::ByteRange;
use crate::engine::s2t::S2TConverter;
use crate::engine::scan::{is_spaced_acronym_issue, ContentType, Scanner};
#[cfg(feature = "translate")]
use crate::engine::translate::calibrate_issues;
use crate::engine::zhtype::{detect_chinese_type, ChineseType};
use crate::fixer::{
    apply_fixes_with_context, remap_to_post_fix, suppress_convergent_issues, FixMode,
};
use crate::rules::loader::compute_ruleset_hash;
use crate::rules::ruleset::Ruleset;
use crate::rules::ruleset::{Issue, IssueType, PoliticalStance, Profile, ResolutionTier, Severity};
use crate::rules::store::{OverrideStore, PackStore, SuppressionStore, TranslationMemoryStore};

/// The MCP tool server. Holds the compiled scanner, override/pack stores,
/// ruleset metadata, and client capability information.
pub struct Server {
    scanner: Scanner,
    /// SC→TC converter for auto-converting Simplified Chinese input.
    /// Built lazily on first Simplified input: its automaton costs ~200ms to
    /// construct, which would otherwise sit on the startup/handshake path, and
    /// traditional-only sessions never need it at all.
    s2t: OnceCell<S2TConverter>,
    suppression_store: SuppressionStore,
    /// Translation memory: persistent correction tracking.
    tm_store: Option<TranslationMemoryStore>,
    ruleset_hash: String,
    /// Span-level judgment cache for persistent LLM disambiguation results (51.4).
    judgment_cache: crate::rules::judgment_cache::JudgmentCache,
    /// Parsed client capabilities from the initialize handshake.
    client_capabilities: ClientCapabilities,
    /// Whether the client has completed the initialize handshake.
    initialized: bool,
    /// Whether the client has sent a shutdown request.
    shutdown_requested: bool,
    /// Client name from initialize handshake, used for auto-compact detection.
    client_name: Option<String>,
}

impl Server {
    /// Create a new server from the embedded ruleset + override/pack stores.
    pub fn new(
        store: OverrideStore,
        suppression_store: SuppressionStore,
        pack_store: PackStore,
        active_packs: Vec<String>,
        tm_store: Option<TranslationMemoryStore>,
    ) -> anyhow::Result<Self> {
        let base_ruleset = crate::rules::loader::load_embedded_ruleset()?;

        let (scanner, ruleset_hash) =
            Self::build_scanner(&base_ruleset, &store, &pack_store, &active_packs)?;

        let judgment_cache = crate::rules::judgment_cache::JudgmentCache::open_default();

        Ok(Self {
            scanner,
            s2t: OnceCell::new(),
            suppression_store,
            tm_store,
            ruleset_hash,
            judgment_cache,
            client_capabilities: ClientCapabilities::default(),
            initialized: false,
            shutdown_requested: false,
            client_name: None,
        })
    }

    /// Build a scanner from the base ruleset, overrides, and active packs.
    fn build_scanner(
        base_ruleset: &Ruleset,
        store: &OverrideStore,
        pack_store: &PackStore,
        active_packs: &[String],
    ) -> anyhow::Result<(Scanner, String)> {
        let (merged_spelling, merged_case) = crate::rules::store::build_merged_rules(
            &base_ruleset.spelling_rules,
            &base_ruleset.case_rules,
            store,
            pack_store,
            active_packs,
        );

        let ruleset_hash = compute_ruleset_hash(&merged_spelling, &merged_case);
        let scanner = Scanner::new(merged_spelling, merged_case);

        Ok((scanner, ruleset_hash))
    }

    /// Whether the client declared sampling support during initialization.
    pub(crate) fn supports_sampling(&self) -> bool {
        self.client_capabilities.sampling
    }

    pub(crate) fn supports_logging(&self) -> bool {
        self.client_capabilities.logging
    }

    /// Handle pre-initialization routing shared between sync and async transports.
    ///
    /// Returns `Some(response)` if the method was handled (initialize, ping,
    /// notifications, or rejection before init). Returns `None` if the caller
    /// should proceed with post-init method dispatch.
    pub(crate) fn dispatch_preinit(
        &mut self,
        req: &mut JsonRpcRequest,
    ) -> Option<Option<JsonRpcResponse>> {
        // exit is always honored regardless of lifecycle state.
        if req.method == "exit" {
            tracing::info!("exit notification, terminating");
            // Flush judgment cache before exit (process::exit skips Drop).
            self.judgment_cache.flush();
            // MCP spec: unconditional process exit.
            // Exit code 0 if shutdown was requested first, 1 otherwise.
            let code = if self.shutdown_requested { 0 } else { 1 };
            std::process::exit(code);
        }

        // After shutdown, reject everything except exit (handled above).
        if self.shutdown_requested {
            tracing::warn!("rejecting {} after shutdown", req.method);
            return Some(if req.id.is_some() {
                Some(JsonRpcResponse::error(
                    req.id.clone(),
                    INVALID_REQUEST,
                    "server is shutting down".into(),
                ))
            } else {
                None
            });
        }

        match req.method.as_str() {
            "initialize" => {
                if req.id.is_none() {
                    tracing::warn!("initialize sent as notification, ignoring");
                    return Some(None);
                }
                if self.initialized {
                    tracing::warn!("duplicate initialize request, rejecting");
                    return Some(Some(JsonRpcResponse::error(
                        req.id.clone(),
                        INVALID_REQUEST,
                        "already initialized".into(),
                    )));
                }
                Some(Some(self.handle_initialize(req)))
            }
            "notifications/cancelled" => {
                tracing::info!("{}", req.method);
                if req.id.is_some() {
                    Some(Some(JsonRpcResponse::error(
                        req.id.clone(),
                        INVALID_REQUEST,
                        "notifications/cancelled must be sent as a notification (no id)".into(),
                    )))
                } else {
                    Some(None)
                }
            }
            "notifications/initialized" => {
                tracing::info!("{}", req.method);
                if req.id.is_some() {
                    Some(Some(JsonRpcResponse::error(
                        req.id.clone(),
                        INVALID_REQUEST,
                        "notifications/initialized must be sent as a notification (no id)".into(),
                    )))
                } else {
                    Some(None)
                }
            }
            "shutdown" => {
                tracing::info!("shutdown requested");
                self.shutdown_requested = true;
                if req.id.is_some() {
                    Some(Some(JsonRpcResponse::success(
                        req.id.clone(),
                        serde_json::json!({}),
                    )))
                } else {
                    // shutdown as notification: set flag, no response
                    Some(None)
                }
            }
            "ping" => {
                if req.id.is_some() {
                    Some(Some(JsonRpcResponse::success(
                        req.id.clone(),
                        serde_json::json!({}),
                    )))
                } else {
                    tracing::debug!("ping sent as notification, ignoring");
                    Some(None)
                }
            }
            _ if !self.initialized => {
                tracing::warn!("rejecting {} before initialization", req.method);
                Some(if req.id.is_some() {
                    Some(JsonRpcResponse::error(
                        req.id.clone(),
                        SERVER_NOT_INITIALIZED,
                        "server not initialized".into(),
                    ))
                } else {
                    None
                })
            }
            _ => None, // proceed to post-init dispatch
        }
    }

    /// Route a post-init method call (no sampling bridge).
    ///
    /// Shared between both transports for tools/list, resources, prompts, etc.
    /// tools/call is handled separately in the sync transport (needs bridge).
    pub(crate) fn dispatch_method(&mut self, req: &mut JsonRpcRequest) -> Option<JsonRpcResponse> {
        match req.method.as_str() {
            "tools/list" => Some(self.handle_tools_list(req)),
            "resources/list" => Some(self.handle_resources_list(req)),
            "resources/read" => Some(self.handle_resources_read(req)),
            "prompts/list" => Some(self.handle_prompts_list(req)),
            "prompts/get" => Some(self.handle_prompts_get(req)),
            _ => {
                tracing::debug!("unhandled method: {}", req.method);
                if req.id.is_some() {
                    Some(JsonRpcResponse::error(
                        req.id.clone(),
                        METHOD_NOT_FOUND,
                        format!("unknown method: {}", req.method),
                    ))
                } else {
                    None
                }
            }
        }
    }

    // MCP method handlers

    pub fn handle_initialize(&mut self, req: &mut JsonRpcRequest) -> JsonRpcResponse {
        let params: InitializeParams = match parse_params(req, "initialize") {
            Ok(p) => p,
            Err(resp) => return *resp,
        };
        let _span = tracing::info_span!("mcp_request", method = "initialize").entered();

        // Store parsed client capabilities for later use (e.g. sampling).
        self.client_capabilities = ClientCapabilities::from(&params.capabilities);
        self.client_name = params.client_info.map(|ci| ci.name);
        self.initialized = true;

        let result = InitializeResult {
            protocol_version: MCP_PROTOCOL_VERSION,
            capabilities: ServerCapabilities {
                tools: ToolCapability {
                    list_changed: false,
                },
                resources: ResourceCapability {
                    list_changed: false,
                },
                prompts: PromptCapability {
                    list_changed: false,
                },
            },
            server_info: ServerInfo {
                name: "zhtw-mcp".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        };

        json_response(req.id.clone(), result)
    }

    pub fn handle_tools_list(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        let tools = ToolsListResult {
            tools: tool_definitions(),
        };
        json_response(req.id.clone(), tools)
    }

    pub(crate) fn handle_tools_call(
        &mut self,
        req: &mut JsonRpcRequest,
        bridge: Option<&mut SamplingBridge<'_>>,
    ) -> JsonRpcResponse {
        let _span = tracing::info_span!("mcp_request", method = "tools/call").entered();
        let params: CallToolParams = match parse_params(req, "tools/call") {
            Ok(p) => p,
            Err(resp) => return *resp,
        };

        if params.name == "zhtw" {
            if !params.arguments.is_object() {
                let actual = json_type_name(&params.arguments);
                return JsonRpcResponse::error_with_data(
                    req.id.clone(),
                    INVALID_PARAMS,
                    format!("arguments must be an object, got {actual}"),
                    json!({ "field": "arguments", "expected_type": "object", "actual_type": actual }),
                );
            }
            if let Some(resp) = reject_unknown_params(&params.arguments, req.id.clone()) {
                return resp;
            }
        }

        let result = match params.name.as_str() {
            "zhtw" => match self.tool_check(&params.arguments, bridge, req.id.clone()) {
                Ok(r) => r,
                Err(resp) => return *resp,
            },
            _ => CallToolResult::error(format!("unknown tool: {}", params.name)),
        };

        json_response(req.id.clone(), result)
    }

    // Tool implementation

    /// Maximum allowed size of the text field (256 KiB). Requests exceeding
    /// this trigger a structured error before any processing begins.
    const MAX_TEXT_BYTES: usize = 256 * 1024;

    fn tool_check(
        &mut self,
        args: &Value,
        mut bridge: Option<&mut SamplingBridge<'_>>,
        id: Option<super::types::RequestId>,
    ) -> ParamResult<CallToolResult> {
        let started = std::time::Instant::now();
        // Snapshot cache counters at start for per-request telemetry.
        let cache_hits_before = self.judgment_cache.hits;
        let cache_misses_before = self.judgment_cache.misses;

        let text = require_str_validated(args, "text", &id)?;

        if text.len() > Self::MAX_TEXT_BYTES {
            return Err(param_error(
                &id,
                "text",
                &format!("{} bytes", text.len()),
                &[&format!("<= {} bytes (256 KiB)", Self::MAX_TEXT_BYTES)],
            ));
        }

        // Auto-detect Simplified Chinese and convert to Traditional via S2T.
        let s2t_converted: Option<String> = if detect_chinese_type(text) == ChineseType::Simplified
        {
            Some(self.s2t.get_or_init(S2TConverter::new).convert(text))
        } else {
            None
        };
        let text = s2t_converted.as_deref().unwrap_or(text);

        let fix_mode = parse_fix_mode(args, &id)?;
        let profile = parse_profile(args, &id)?;
        let content_type = parse_content_type(args, &id)?;
        let stance = parse_political_stance(args, &id)?;
        let max_errors = args.get("max_errors").and_then(|v| v.as_u64());
        let max_warnings = args.get("max_warnings").and_then(|v| v.as_u64());
        let ignore_terms = parse_ignore_terms(args);
        let ignore_set: std::collections::HashSet<&str> =
            ignore_terms.iter().map(String::as_str).collect();
        let explain = parse_explain(args);
        let output_mode =
            parse_output_mode(args, default_output_mode(self.client_name.as_deref()), &id)?;
        let fix_output = parse_fix_output(args, &id)?;
        #[cfg(feature = "translate")]
        let verify = parse_verify(args);

        let stance_name = stance.unwrap_or(PoliticalStance::RocCentric).name();
        // Explicit bool overrides default; absent means inherit profile default.
        // Default profile enables both ai_filler_detection and
        // translationese_detection.
        let detect_ai_opt = args.get("detect_ai").and_then(|v| v.as_bool());
        let detect_translationese_opt = args.get("detect_translationese").and_then(|v| v.as_bool());
        // Explicit opt-in for the composite three-axis scorecard —
        // mirrors the CLI `--detect-style` shorthand, off-by-default to
        // keep the standard payload lean.
        let detect_style = args
            .get("detect_style")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let translationese_domain_opt = args
            .get("translationese_domain")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let ai_threshold = optional_str_validated(args, "ai_threshold", &id)?;

        let relaxed = args
            .get("relaxed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let exempt_blockquotes = args
            .get("exempt_blockquotes")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let glossary = parse_glossary(args);

        let consistency_requested = args
            .get("consistency")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let include_telemetry = args
            .get("include_telemetry")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let include_stats = args
            .get("include_stats")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let _span = tracing::info_span!(
            "tool_check",
            content_length = text.len() as u64,
            content_type = content_type.name(),
            profile = profile.name()
        )
        .entered();

        if include_telemetry && output_mode == OutputMode::Tabular {
            return Err(param_error(
                &id,
                "include_telemetry",
                "true",
                &["false", "or use output=full|compact|summary"],
            ));
        }

        if include_stats && output_mode == OutputMode::Tabular {
            return Err(param_error(
                &id,
                "include_stats",
                "true",
                &["false", "or use output=full|compact|summary"],
            ));
        }

        if detect_style && output_mode == OutputMode::Tabular {
            return Err(param_error(
                &id,
                "detect_style",
                "true",
                &["false", "or use output=full|compact|summary"],
            ));
        }

        // Build effective config: profile base + capability flags.
        let mut cfg = profile.config();
        if relaxed {
            cfg = cfg.with_relaxed();
        }
        if exempt_blockquotes {
            cfg = cfg.with_exempt_blockquotes(true);
        }
        if let Some(st) = stance {
            cfg = cfg.with_stance(st);
        }
        // `detect_style` mirrors the CLI shorthand: it always computes the
        // full three-axis scorecard, regardless of explicit per-axis disables.
        if detect_style {
            cfg.translationese_detection = true;
        } else if let Some(b) = detect_translationese_opt {
            cfg.translationese_detection = b;
        }
        if let Some(domain_str) = &translationese_domain_opt {
            match crate::engine::translationese_score::TranslationeseDomain::from_str_strict(
                domain_str,
            ) {
                Some(d) => cfg.translationese_domain = d,
                None => {
                    return Err(param_error(
                        &id,
                        "translationese_domain",
                        domain_str,
                        &["general", "technical", "literary", "news"],
                    ));
                }
            }
        }
        // Resolve effective AI detection: explicit arg wins over profile default.
        // All four AI sub-flags move as a unit — enabling detection turns them
        // all on, disabling turns them all off.
        let detect_ai = if detect_style {
            true
        } else {
            detect_ai_opt.unwrap_or(cfg.ai_filler_detection)
        };
        cfg.ai_filler_detection = detect_ai;
        cfg.ai_semantic_safety = detect_ai;
        cfg.ai_density_detection = detect_ai;
        cfg.ai_structural_patterns = detect_ai;
        if detect_ai {
            // Apply threshold level: low=0.5 (sensitive), medium=1.0, high=1.5 (conservative).
            cfg.ai_threshold_multiplier = match ai_threshold {
                Some("low") => 0.5,
                Some("medium") | None => 1.0,
                Some("high") => 1.5,
                Some(other) => {
                    return Err(param_error(
                        &id,
                        "ai_threshold",
                        other,
                        &["low", "medium", "high"],
                    ));
                }
            };
        }

        let apply_glossary_to_issues = |work_text: &str, issues: Vec<Issue>| -> Vec<Issue> {
            if glossary.is_empty() {
                return issues;
            }
            let md_opts = crate::engine::markdown::MdScanOptions::new(
                matches!(
                    content_type,
                    crate::engine::scan::ContentType::MarkdownScanCode
                ),
                cfg.exempt_blockquotes,
            );
            let excluded = crate::engine::scan::build_exclusions_for_content_type_with_options(
                work_text,
                content_type,
                md_opts,
            );
            let mut issues =
                crate::rules::glossary::apply_glossary(work_text, &excluded, issues, &glossary);
            let line_index = crate::engine::lineindex::LineIndex::new(work_text);
            line_index
                .fill_line_col_sorted(&mut issues, crate::engine::lineindex::ColumnEncoding::Utf16);
            issues
        };

        let result = match fix_mode {
            FixMode::None => {
                // Lint-only path.
                let output =
                    self.scanner
                        .scan_for_content_type_with_config(text, content_type, cfg);
                let detected_script = if s2t_converted.is_some() {
                    "simplified"
                } else {
                    output.detected_script.name()
                };
                let coverage = output.coverage.as_ref();
                let oral_density = output.oral_density;
                let quality_flags = &output.quality_flags;
                let ai_signature = output.ai_signature;
                let translationese_signature = output.translationese_signature;
                let mut issues = output.issues;
                let scanner_hit_count = issues.len();
                if let Some(st) = stance {
                    filter_by_stance(&mut issues, st);
                }
                // Calibrate issues via Google Translate anchor matching.
                #[cfg(feature = "translate")]
                let calibrate_result = if verify {
                    Some(calibrate_issues(text, &mut issues))
                } else {
                    None
                };

                // Tier 2: local disambiguation.  Resolves issues via context
                // clues, profile priors, and collocations before LLM sampling.
                let disambig_cfg = DisambigConfig {
                    profile,
                    ..Default::default()
                };
                let disambig_stats = disambiguate_batch(&mut issues, text, &disambig_cfg);

                // Tier 3: LLM sampling for gray-zone issues only.
                let sampling_stats = if let Some(b) = bridge.as_mut() {
                    let mut cache_ctx = super::sampling::SamplingCacheCtx {
                        cache: &mut self.judgment_cache,
                        ruleset_hash: &self.ruleset_hash,
                        profile: profile.name(),
                        content_type: content_type.name(),
                    };
                    refine_issues_with_sampling(&mut issues, b, text, Some(&mut cache_ctx))
                } else {
                    SamplingStats::default()
                };
                self.apply_suppressions(&mut issues);
                let tm_suppressed = self.apply_tm(&mut issues);
                apply_ignore_set(&mut issues, &ignore_set);

                // 35.9 — Apply project glossary precedence (banned > TM):
                // proper_nouns suppress, banned inject synthetic Errors.
                // Recompute exclusion ranges so banned-term scanning
                // respects code blocks, URLs, and frontmatter.  Re-fill
                // line/col on synthetic issues.
                issues = apply_glossary_to_issues(text, issues);

                // 35.1 — Document-wide consistency report.
                let consistency_report = consistency_requested
                    .then(|| {
                        crate::engine::consistency::compute_consistency_report(
                            text, &issues, &glossary,
                        )
                    })
                    .filter(|r| !r.is_empty());

                // Build telemetry if requested.
                let telemetry = if include_telemetry {
                    Some(build_telemetry(
                        text,
                        scanner_hit_count,
                        &disambig_stats,
                        &sampling_stats,
                        bridge.as_ref(),
                        0,
                        (
                            self.judgment_cache.hits.saturating_sub(cache_hits_before),
                            self.judgment_cache
                                .misses
                                .saturating_sub(cache_misses_before),
                        ),
                    ))
                } else {
                    None
                };

                let trace =
                    Trace::new("zhtw", &self.ruleset_hash, text).with_issue_count(issues.len());

                // Pre-build the composite scorecard so its lifetime spans
                // the build_check_output call (the params struct only
                // borrows it).
                let style_scorecard = style_scorecard_for(
                    detect_style,
                    ai_signature.as_ref(),
                    translationese_signature.as_ref(),
                    &issues,
                    text,
                );

                build_check_output(&CheckOutputParams {
                    result_text: text,
                    issues: &issues,
                    applied_fixes: 0,
                    max_errors,
                    max_warnings,
                    profile,
                    stance_name,
                    detected_script,
                    s2t_applied: s2t_converted.is_some(),
                    trace: &trace,
                    explain,
                    output_mode,
                    has_fixes: s2t_converted.is_some(),
                    fix_output,
                    original_text: text,
                    fix_records: &[],
                    #[cfg(feature = "translate")]
                    calibrate_result,
                    coverage,
                    oral_density,
                    quality_flags,
                    ai_signature: ai_signature.as_ref(),
                    translationese_signature: translationese_signature.as_ref(),
                    style_scorecard: style_scorecard.as_ref(),
                    tm_suppressed,
                    sampling_stats,
                    disambig_stats,
                    telemetry,
                    include_stats,
                    consistency: consistency_report.as_ref(),
                })
            }

            mode @ (FixMode::Orthographic | FixMode::LexicalSafe | FixMode::LexicalContextual) => {
                // Fix path: scan, apply fixes, re-scan for residual issues.
                let md_opts = crate::engine::markdown::MdScanOptions::new(
                    matches!(
                        content_type,
                        crate::engine::scan::ContentType::MarkdownScanCode
                    ),
                    cfg.exempt_blockquotes,
                );
                let excluded = crate::engine::scan::build_exclusions_for_content_type_with_options(
                    text,
                    content_type,
                    md_opts,
                );
                let scan_out = self.scanner.scan_with_prebuilt_excluded_config(
                    text,
                    &excluded,
                    cfg,
                    content_type,
                );
                let detected_script = if s2t_converted.is_some() {
                    "simplified"
                } else {
                    scan_out.detected_script.name()
                };
                let mut issues = scan_out.issues;
                let scanner_hit_count = issues.len();
                if let Some(st) = stance {
                    filter_by_stance(&mut issues, st);
                }

                // Calibrate issues via Google Translate anchor matching.
                #[cfg(feature = "translate")]
                let calibrate_result = if verify {
                    Some(calibrate_issues(text, &mut issues))
                } else {
                    None
                };

                // Tier 2: local disambiguation.
                let disambig_cfg = DisambigConfig {
                    profile,
                    ..Default::default()
                };
                let disambig_stats = disambiguate_batch(&mut issues, text, &disambig_cfg);

                // Tier 3: LLM sampling for gray-zone issues only.
                let sampling_stats = if let Some(b) = bridge.as_mut() {
                    let mut cache_ctx = super::sampling::SamplingCacheCtx {
                        cache: &mut self.judgment_cache,
                        ruleset_hash: &self.ruleset_hash,
                        profile: profile.name(),
                        content_type: content_type.name(),
                    };
                    refine_issues_with_sampling(&mut issues, b, text, Some(&mut cache_ctx))
                } else {
                    SamplingStats::default()
                };

                self.apply_suppressions(&mut issues);
                // TM is NOT applied here: the fixer filter (should_suppress)
                // prevents fixing TM-rejected terms, and the post-fix apply_tm
                // handles severity downgrade + counting on the final residual.
                apply_ignore_set(&mut issues, &ignore_set);
                issues = apply_glossary_to_issues(text, issues);

                // Snapshot AFTER suppressions so restored severity reflects final state.
                struct PreservedState {
                    term: String,
                    orig_offset: usize,
                    length: usize,
                    english: Option<Arc<str>>,
                    severity: Severity,
                    anchor_match: Option<bool>,
                    context: Option<Arc<str>>,
                    suggestions: Vec<String>,
                }

                let preserved_states: Vec<PreservedState> = issues
                    .iter()
                    .map(|i| PreservedState {
                        term: i.found.clone(),
                        orig_offset: i.offset,
                        length: i.length,
                        english: i.english.clone(),
                        severity: i.severity,
                        anchor_match: i.anchor_match,
                        context: i.context.clone(),
                        suggestions: i.suggestions.to_vec(),
                    })
                    .collect();

                // Filter out TM-suppressed issues before fixing: a term the
                // user deliberately rejected must not be auto-corrected.
                let fix_issues: Vec<Issue> = match &self.tm_store {
                    Some(tm) => issues
                        .iter()
                        .filter(|i| !tm.should_suppress(&i.found))
                        .cloned()
                        .collect(),
                    None => issues.clone(),
                };

                let excluded_pairs = to_offset_pairs(&excluded);
                let fix_result = apply_fixes_with_context(
                    text,
                    &fix_issues,
                    mode,
                    &excluded_pairs,
                    Some(self.scanner.segmenter()),
                );

                // Re-scan after fixes — use post-fix ai_signature, not pre-fix.
                // Remap exclusion zones to post-fix coordinates instead of
                // rebuilding from scratch (avoids re-parsing markdown/URLs on
                // the entire document for every fix cycle).
                let remapped_excl =
                    crate::fixer::remap_exclusions(&excluded, &fix_result.applied_fixes);
                let rescan_out = self.scanner.scan_with_prebuilt_excluded_config(
                    &fix_result.text,
                    &remapped_excl,
                    cfg,
                    content_type,
                );
                let coverage = rescan_out.coverage.as_ref();
                let oral_density = rescan_out.oral_density;
                let quality_flags = &rescan_out.quality_flags;
                let ai_signature = rescan_out.ai_signature;
                let translationese_signature = rescan_out.translationese_signature;
                let mut remaining_issues = rescan_out.issues;
                if let Some(st) = stance {
                    filter_by_stance(&mut remaining_issues, st);
                }
                self.apply_suppressions(&mut remaining_issues);
                apply_ignore_set(&mut remaining_issues, &ignore_set);

                // Precompute remapped offsets once (O(M*F)) and index by
                // post-fix offset for O(1) lookup per remaining issue.
                use rustc_hash::FxHashMap;
                let mut state_by_offset: FxHashMap<usize, Vec<usize>> =
                    FxHashMap::with_capacity_and_hasher(preserved_states.len(), Default::default());
                for (idx, state) in preserved_states.iter().enumerate() {
                    let remapped = remap_to_post_fix(state.orig_offset, &fix_result.applied_fixes);
                    state_by_offset.entry(remapped).or_default().push(idx);
                }

                // Re-apply preserved states using identity-safe matching:
                // term + remapped offset + length + english must all match.
                for issue in &mut remaining_issues {
                    if let Some(candidates) = state_by_offset.get(&issue.offset) {
                        if let Some(&idx) = candidates.iter().find(|&&idx| {
                            let s = &preserved_states[idx];
                            s.term == issue.found
                                && s.length == issue.length
                                && s.english == issue.english
                        }) {
                            let state = &preserved_states[idx];
                            issue.severity = state.severity;
                            issue.anchor_match = state.anchor_match;
                            issue.context = state.context.clone();
                            issue.suggestions = state.suggestions.clone().into();
                            issue.refresh_suggested_rewrite();
                        }
                    }
                }

                // Suppress convergent-chain noise: remove re-scan issues
                // whose offset falls within a byte range written by the fixer.
                suppress_convergent_issues(&mut remaining_issues, &fix_result.applied_fixes);

                remaining_issues = apply_glossary_to_issues(&fix_result.text, remaining_issues);

                // Apply TM after preserved state restoration so the count
                // reflects the true final state, not a pre-fix snapshot.
                let tm_suppressed = self.apply_tm(&mut remaining_issues);

                let consistency_report = consistency_requested
                    .then(|| {
                        crate::engine::consistency::compute_consistency_report(
                            &fix_result.text,
                            &remaining_issues,
                            &glossary,
                        )
                    })
                    .filter(|r| !r.is_empty());

                // Build telemetry if requested.
                let telemetry = if include_telemetry {
                    Some(build_telemetry(
                        text,
                        scanner_hit_count,
                        &disambig_stats,
                        &sampling_stats,
                        bridge.as_ref(),
                        fix_result.applied,
                        (
                            self.judgment_cache.hits.saturating_sub(cache_hits_before),
                            self.judgment_cache
                                .misses
                                .saturating_sub(cache_misses_before),
                        ),
                    ))
                } else {
                    None
                };

                let trace = Trace::new("zhtw", &self.ruleset_hash, text)
                    .with_issue_count(remaining_issues.len())
                    .with_output(&fix_result.text);

                // Composite scorecard against the post-fix text and
                // remaining issues, so the scorecard reflects the
                // user-visible state.
                let style_scorecard = style_scorecard_for(
                    detect_style,
                    ai_signature.as_ref(),
                    translationese_signature.as_ref(),
                    &remaining_issues,
                    &fix_result.text,
                );

                build_check_output(&CheckOutputParams {
                    result_text: &fix_result.text,
                    issues: &remaining_issues,
                    applied_fixes: fix_result.applied,
                    max_errors,
                    max_warnings,
                    profile,
                    stance_name,
                    detected_script,
                    s2t_applied: s2t_converted.is_some(),
                    trace: &trace,
                    explain,
                    output_mode,
                    has_fixes: fix_result.applied > 0 || s2t_converted.is_some(),
                    fix_output,
                    original_text: text,
                    fix_records: &fix_result.applied_fixes,
                    #[cfg(feature = "translate")]
                    calibrate_result,
                    coverage,
                    oral_density,
                    quality_flags,
                    ai_signature: ai_signature.as_ref(),
                    translationese_signature: translationese_signature.as_ref(),
                    style_scorecard: style_scorecard.as_ref(),
                    tm_suppressed,
                    sampling_stats,
                    disambig_stats,
                    telemetry,
                    include_stats,
                    consistency: consistency_report.as_ref(),
                })
            }
        };
        tracing::info!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            "tool_check completed"
        );
        Ok(result)
    }

    /// Downgrade suppressed issues to Info severity.
    fn apply_suppressions(&self, issues: &mut [Issue]) {
        for issue in issues {
            if self.suppression_store.is_suppressed(&issue.found) {
                issue.severity = Severity::Info;
            }
        }
    }

    /// Apply translation memory: suppress lexical/contextual issues that the
    /// user previously rejected (kept the flagged term). Orthographic issue
    /// types (Punctuation, Case, Variant, Grammar, AiStyle) are immune.
    /// Returns the number of issues suppressed.
    fn apply_tm(&self, issues: &mut [Issue]) -> usize {
        let Some(tm) = &self.tm_store else {
            return 0;
        };
        let mut count = 0;
        for issue in issues {
            match issue.rule_type {
                IssueType::Punctuation
                | IssueType::Case
                | IssueType::Variant
                | IssueType::Grammar
                | IssueType::AiStyle => continue,
                _ => {}
            }
            // Glossary-banned terms are project-wide truth (banned > TM
            // per the documented precedence).  The provenance tag is
            // set by `apply_glossary` either by injecting a synthetic
            // Error or by upgrading a covering issue; either way TM
            // must not downgrade these.
            let is_glossary_banned = crate::rules::glossary::is_glossary_banned(issue);
            if is_glossary_banned {
                continue;
            }
            if tm.should_suppress(&issue.found) && issue.severity != Severity::Info {
                issue.severity = Severity::Info;
                count += 1;
            }
        }
        count
    }

    // -- Resource and prompt handlers -----------------------------------------

    pub fn handle_resources_list(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        json_response(req.id.clone(), resources::list_resources())
    }

    pub fn handle_resources_read(&self, req: &mut JsonRpcRequest) -> JsonRpcResponse {
        let params: ResourceReadParams = match parse_params(req, "resources/read") {
            Ok(p) => p,
            Err(resp) => return *resp,
        };

        match resources::read_resource(&params.uri, self.scanner.spelling_rules()) {
            Some(result) => json_response(req.id.clone(), result),
            None => JsonRpcResponse::error(
                req.id.clone(),
                INVALID_PARAMS,
                format!("unknown resource URI: {}", params.uri),
            ),
        }
    }

    pub fn handle_prompts_list(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        let result = prompts::list_prompts();
        json_response(req.id.clone(), json!({ "prompts": result }))
    }

    pub fn handle_prompts_get(&self, req: &mut JsonRpcRequest) -> JsonRpcResponse {
        let params: PromptGetParams = match parse_params(req, "prompts/get") {
            Ok(p) => p,
            Err(resp) => return *resp,
        };

        let prompt_args: std::collections::HashMap<String, String> = params
            .arguments
            .as_object()
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        match prompts::get_prompt(&params.name, &prompt_args) {
            Some(result) => json_response(req.id.clone(), result),
            None => JsonRpcResponse::error(
                req.id.clone(),
                INVALID_PARAMS,
                format!("unknown prompt: {}", params.name),
            ),
        }
    }
}

/// Serialize result to JSON and wrap in a success response, or return an
/// internal error response on serialization failure.
fn json_response(
    id: Option<super::types::RequestId>,
    result: impl serde::Serialize,
) -> JsonRpcResponse {
    match serde_json::to_value(result) {
        Ok(v) => JsonRpcResponse::success(id, v),
        Err(e) => {
            tracing::error!("failed to serialize response: {e}");
            JsonRpcResponse::error(id, INTERNAL_ERROR, "internal server error".into())
        }
    }
}

/// Convert excluded byte ranges to the (start, end) pairs expected by apply_fixes.
fn to_offset_pairs(ranges: &[ByteRange]) -> Vec<(usize, usize)> {
    ranges.iter().map(|r| (r.start, r.end)).collect()
}

/// Result of parsing a request or tool argument: the error side carries a
/// ready-to-send JSON-RPC response.
///
/// The error is boxed because `JsonRpcResponse` is large, and an unboxed
/// `Result` is sized for its largest variant, so every successful parse paid
/// for an error that almost never happens. Boxing moves that cost to the
/// failure branch and removes the `clippy::result_large_err` suppressions
/// that used to sit on each of these helpers.
type ParamResult<T> = Result<T, Box<JsonRpcResponse>>;

/// Parse and take MCP request params, returning a typed struct or an error response.
fn parse_params<T: serde::de::DeserializeOwned>(
    req: &mut JsonRpcRequest,
    method: &str,
) -> ParamResult<T> {
    serde_json::from_value(std::mem::take(&mut req.params)).map_err(|e| {
        tracing::warn!("bad {method} params: {e}");
        Box::new(JsonRpcResponse::error(
            req.id.clone(),
            INVALID_PARAMS,
            format!("invalid {method} parameters"),
        ))
    })
}

/// Known parameter names for the `zhtw` tool, kept in sync with
/// `tool_definitions()` schema properties. Any key in `arguments` not in this
/// set triggers INVALID_PARAMS (-32602) with structured `data.unexpected`.
fn zhtw_known_params() -> &'static [&'static str] {
    #[cfg(feature = "translate")]
    {
        &[
            "text",
            "fix_mode",
            "max_errors",
            "max_warnings",
            "profile",
            "relaxed",
            "exempt_blockquotes",
            "content_type",
            "political_stance",
            "ignore_terms",
            "glossary",
            "consistency",
            "explain",
            "fix_output",
            "verify",
            "output",
            "detect_ai",
            "detect_translationese",
            "detect_style",
            "translationese_domain",
            "ai_threshold",
            "include_telemetry",
            "include_stats",
        ]
    }
    #[cfg(not(feature = "translate"))]
    {
        &[
            "text",
            "fix_mode",
            "max_errors",
            "max_warnings",
            "profile",
            "relaxed",
            "exempt_blockquotes",
            "content_type",
            "political_stance",
            "ignore_terms",
            "glossary",
            "consistency",
            "explain",
            "fix_output",
            "output",
            "detect_ai",
            "detect_translationese",
            "detect_style",
            "translationese_domain",
            "ai_threshold",
            "include_telemetry",
            "include_stats",
        ]
    }
}

/// Return an INVALID_PARAMS JSON-RPC error if `args` contains keys not in
/// the known parameter set. Returns `None` when all keys are recognized.
fn reject_unknown_params(
    args: &Value,
    id: Option<super::types::RequestId>,
) -> Option<JsonRpcResponse> {
    let obj = args.as_object()?;
    let known = zhtw_known_params();
    let unexpected: Vec<&str> = obj
        .keys()
        .filter(|k| !known.contains(&k.as_str()))
        .map(String::as_str)
        .collect();
    if unexpected.is_empty() {
        return None;
    }
    Some(JsonRpcResponse::error_with_data(
        id,
        INVALID_PARAMS,
        format!(
            "unknown parameter{}: {}",
            if unexpected.len() > 1 { "s" } else { "" },
            unexpected.join(", "),
        ),
        json!({ "unexpected": unexpected }),
    ))
}

/// Build a structured INVALID_PARAMS JSON-RPC error for a bad tool parameter.
/// The `data` field carries `{"field", "value", "accepted"}` so clients can
/// render actionable diagnostics without parsing the message string.
fn param_error(
    id: &Option<super::types::RequestId>,
    field: &str,
    value: &str,
    accepted: &[&str],
) -> Box<JsonRpcResponse> {
    Box::new(JsonRpcResponse::error_with_data(
        id.clone(),
        INVALID_PARAMS,
        format!("invalid '{field}': '{value}'"),
        json!({ "field": field, "value": value, "accepted": accepted }),
    ))
}

/// Extract a required string field from a JSON object, returning a
/// structured INVALID_PARAMS error on failure. Distinguishes missing
/// field from present-but-wrong-type so clients get actionable diagnostics.
fn require_str_validated<'a>(
    args: &'a Value,
    field: &str,
    id: &Option<super::types::RequestId>,
) -> ParamResult<&'a str> {
    match args.get(field) {
        None => Err(Box::new(JsonRpcResponse::error_with_data(
            id.clone(),
            INVALID_PARAMS,
            format!("missing required parameter '{field}'"),
            json!({ "field": field }),
        ))),
        Some(v) => v.as_str().ok_or_else(|| {
            let type_name = json_type_name(v);
            Box::new(JsonRpcResponse::error_with_data(
                id.clone(),
                INVALID_PARAMS,
                format!("'{field}' must be a string, got {type_name}"),
                json!({ "field": field, "expected_type": "string", "actual_type": type_name }),
            ))
        }),
    }
}

/// Extract an optional string field, returning INVALID_PARAMS if the
/// value is present but not a string. Returns `Ok(None)` when absent.
fn optional_str_validated<'a>(
    args: &'a Value,
    field: &str,
    id: &Option<super::types::RequestId>,
) -> ParamResult<Option<&'a str>> {
    match args.get(field) {
        None => Ok(None),
        Some(v) => match v.as_str() {
            Some(s) => Ok(Some(s)),
            None => {
                let type_name = json_type_name(v);
                Err(Box::new(JsonRpcResponse::error_with_data(
                    id.clone(),
                    INVALID_PARAMS,
                    format!("'{field}' must be a string, got {type_name}"),
                    json!({ "field": field, "expected_type": "string", "actual_type": type_name }),
                )))
            }
        },
    }
}

/// Human-readable JSON type name for error diagnostics.
fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        Value::Null => "null",
        Value::String(_) => "string",
    }
}

/// Parse the optional "fix_mode" field from tool arguments.
/// Returns an INVALID_PARAMS error for unrecognized values.
fn parse_fix_mode(args: &Value, id: &Option<super::types::RequestId>) -> ParamResult<FixMode> {
    match optional_str_validated(args, "fix_mode", id)? {
        Some("orthographic") => Ok(FixMode::Orthographic),
        Some("lexical_safe") => Ok(FixMode::LexicalSafe),
        Some("lexical_contextual") => Ok(FixMode::LexicalContextual),
        None | Some("none") => Ok(FixMode::None),
        Some(other) => Err(param_error(
            id,
            "fix_mode",
            other,
            &["none", "orthographic", "lexical_safe", "lexical_contextual"],
        )),
    }
}

/// Parse the optional "content_type" field from tool arguments.
/// Returns an INVALID_PARAMS error for unrecognized values.
fn parse_content_type(
    args: &Value,
    id: &Option<super::types::RequestId>,
) -> ParamResult<ContentType> {
    match optional_str_validated(args, "content_type", id)? {
        Some("markdown") => Ok(ContentType::Markdown),
        Some("markdown-scan-code") => Ok(ContentType::MarkdownScanCode),
        Some("yaml") => Ok(ContentType::Yaml),
        Some("plain") | None => Ok(ContentType::Plain),
        Some(other) => Err(param_error(
            id,
            "content_type",
            other,
            &["plain", "markdown", "markdown-scan-code", "yaml"],
        )),
    }
}

/// Parse the optional "profile" field from tool arguments.
/// Returns an INVALID_PARAMS error for unrecognized values.
fn parse_profile(args: &Value, id: &Option<super::types::RequestId>) -> ParamResult<Profile> {
    match optional_str_validated(args, "profile", id)? {
        None => Ok(Profile::Base),
        Some(s) => Profile::from_str_strict(s)
            .ok_or_else(|| param_error(id, "profile", s, &["base", "strict"])),
    }
}

/// Parse the optional "political_stance" field from tool arguments.
/// Returns an INVALID_PARAMS error for unrecognized values.
fn parse_political_stance(
    args: &Value,
    id: &Option<super::types::RequestId>,
) -> ParamResult<Option<PoliticalStance>> {
    match optional_str_validated(args, "political_stance", id)? {
        None => Ok(None),
        Some(s) => PoliticalStance::from_str_strict(s)
            .map(Some)
            .ok_or_else(|| {
                param_error(
                    id,
                    "political_stance",
                    s,
                    &["roc_centric", "international", "neutral"],
                )
            }),
    }
}

/// Fix output format: how corrected text is returned when fixes are applied.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FixOutputMode {
    /// Return the full corrected text (backward compat default).
    Full,
    /// Return search/replace blocks (LLM-friendly patching format).
    SearchReplace,
    /// Return a patches array with byte offsets into the original text.
    Patch,
}

impl FixOutputMode {
    fn name(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::SearchReplace => "search_replace",
            Self::Patch => "patch",
        }
    }
}

/// Parse the optional "fix_output" parameter from tool arguments.
fn parse_fix_output(
    args: &Value,
    id: &Option<super::types::RequestId>,
) -> ParamResult<FixOutputMode> {
    match optional_str_validated(args, "fix_output", id)? {
        Some("full") | None => Ok(FixOutputMode::Full),
        Some("search_replace") => Ok(FixOutputMode::SearchReplace),
        Some("patch") => Ok(FixOutputMode::Patch),
        Some(other) => Err(param_error(
            id,
            "fix_output",
            other,
            &["full", "search_replace", "patch"],
        )),
    }
}

/// Parse the optional "explain" boolean from tool arguments.
fn parse_explain(args: &Value) -> bool {
    args.get("explain")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Output mode for zhtw responses.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    Full,
    Compact,
    /// Header-once TSV format for LLM-facing responses.
    /// Eliminates JSON syntax tax (repeated keys, braces, quotes) that
    /// inflates BPE token count by 40-60% with zero semantic value.
    Tabular,
    /// AI summary only: issue counts + AI signature report.
    /// No individual issues, no text. Lets downstream tools quickly
    /// decide whether to trigger a full review.
    Summary,
}

/// Parse the optional "output" mode from tool arguments.
/// When no explicit value is given, uses the provided default (which may
/// be auto-detected from the client identity).
fn parse_output_mode(
    args: &Value,
    default: OutputMode,
    id: &Option<super::types::RequestId>,
) -> ParamResult<OutputMode> {
    match optional_str_validated(args, "output", id)? {
        Some("compact") => Ok(OutputMode::Compact),
        Some("full") => Ok(OutputMode::Full),
        Some("tabular") => Ok(OutputMode::Tabular),
        Some("summary") => Ok(OutputMode::Summary),
        None => Ok(default),
        Some(other) => Err(param_error(
            id,
            "output",
            other,
            &["full", "compact", "tabular", "summary"],
        )),
    }
}

/// Known AI agent/CLI client names that benefit from compact output.
/// Matched as exact full-name against the lowercased `clientInfo.name`.
/// Only programmatic agents/CLIs — NOT desktop GUI apps like "Claude Desktop".
const AI_AGENT_CLIENTS: &[&str] = &[
    "claude-code",
    "claude code",
    "cursor",
    "cline",
    "continue",
    "zed",
    "windsurf",
    "copilot",
    "aider",
    "cody",
    "roo",
    "roo-code",
    "roo code",
];

/// Determine default output mode from client identity.
/// Uses exact full-name match only to avoid false positives on clients
/// like "Claude Desktop" that happen to share a token with an agent name.
/// Strips trailing version suffixes (`/1.0`, ` 1.0`) before matching,
/// since some clients embed version info in the name field.
fn default_output_mode(client_name: Option<&str>) -> OutputMode {
    match client_name {
        Some(name) => {
            let lower = name.to_ascii_lowercase();
            // Strip trailing version suffix: "Cursor/0.1.0" → "cursor", "cline 1.2" → "cline"
            let base = lower
                .split('/')
                .next()
                .unwrap_or(&lower)
                .trim_end_matches(|c: char| c.is_ascii_digit() || c == '.')
                .trim();
            if AI_AGENT_CLIENTS.contains(&base) {
                OutputMode::Compact
            } else {
                OutputMode::Full
            }
        }
        None => OutputMode::Full,
    }
}

/// Parse the optional "verify" flag from tool arguments.
#[cfg(feature = "translate")]
fn parse_verify(args: &Value) -> bool {
    args.get("verify")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Generate a cultural/linguistic explanation for an issue.
///
/// Draws from the context, english, and rule_type fields to produce
/// a brief explanation useful for AI agents and educational applications.
fn build_explanation(issue: &Issue) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();

    match issue.rule_type {
        IssueType::CrossStrait => {
            if let Some(eng) = &issue.english {
                parts.push(format!(
                    "'{}' is a mainland Chinese term for '{}'; Taiwan uses '{}'.",
                    issue.found,
                    eng,
                    issue.suggestions.join(" / "),
                ));
            } else if !issue.suggestions.is_empty() {
                parts.push(format!(
                    "'{}' is a mainland Chinese expression; Taiwan standard: {}.",
                    issue.found,
                    issue.suggestions.join(" / "),
                ));
            }
        }
        IssueType::Confusable => {
            if let Some(eng) = &issue.english {
                parts.push(format!(
                    "'{}' is ambiguous across the strait. English anchor: '{}'. Taiwan form: {}.",
                    issue.found,
                    eng,
                    issue.suggestions.join(" / "),
                ));
            }
        }
        IssueType::PoliticalColoring => {
            parts.push(format!(
                "'{}' carries mainland political connotations; prefer {}.",
                issue.found,
                issue.suggestions.join(" / "),
            ));
        }
        IssueType::Variant => {
            parts.push(format!(
                "'{}' is a non-standard character variant; MoE standard form: {}.",
                issue.found,
                issue.suggestions.join(" / "),
            ));
        }
        IssueType::Typo => {
            parts.push(format!(
                "'{}' appears to be a typo; suggested: {}.",
                issue.found,
                issue.suggestions.join(" / "),
            ));
        }
        IssueType::Case => {
            parts.push(format!(
                "'{}' has incorrect casing; standard form: {}.",
                issue.found,
                issue.suggestions.join(" / "),
            ));
        }
        IssueType::Punctuation => {
            parts.push(format!(
                "'{}' should use the full-width equivalent {} in CJK prose per MoE standards.",
                issue.found,
                issue.suggestions.join(" / "),
            ));
        }
        IssueType::Grammar => {
            if let Some(ctx) = &issue.context {
                parts.push(format!(
                    "'{}' — {}. Suggested: {}.",
                    issue.found,
                    ctx,
                    issue.suggestions.join(" / "),
                ));
            } else {
                parts.push(format!(
                    "'{}' is a grammatical issue; suggested: {}.",
                    issue.found,
                    issue.suggestions.join(" / "),
                ));
            }
        }
        IssueType::AiStyle => {
            if let Some(ctx) = &issue.context {
                parts.push(format!("'{}' — {}.", issue.found, ctx));
            }
            if !issue.suggestions.is_empty() {
                let sugg = issue.suggestions.join(" / ");
                parts.push(format!("Suggested: {sugg}."));
            } else {
                parts.push("Consider removing or rephrasing.".to_string());
            }
        }
        IssueType::Translationese => {
            if let Some(ctx) = &issue.context {
                parts.push(format!("'{}' — {}.", issue.found, ctx));
            }
            if !issue.suggestions.is_empty() {
                let sugg = issue.suggestions.join(" / ");
                parts.push(format!("Suggested rewrite: {sugg}."));
            } else {
                parts.push(
                    "Translationese / 歐化 pattern; consider an idiomatic zh-TW rewrite."
                        .to_string(),
                );
            }
        }
        IssueType::Repetition => {
            if is_spaced_acronym_issue(issue) {
                parts.push(format!(
                    "'{}' should be written as '{}'; the spacing looks like a transcription artifact.",
                    issue.found,
                    issue.suggestions[0],
                ));
            } else {
                parts.push(format!(
                    "'{}' is a consecutive duplicate; remove the repetition.",
                    issue.found,
                ));
            }
        }
    }

    // Grammar, AiStyle, and Translationese issues already embed context in
    // the main explanation; skip the shared Context: append to avoid
    // duplication.
    if !matches!(
        issue.rule_type,
        IssueType::Grammar | IssueType::AiStyle | IssueType::Translationese
    ) {
        if let Some(ctx) = &issue.context {
            parts.push(format!("Context: {ctx}"));
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

/// Parse the optional "ignore_terms" array from tool arguments.
fn parse_ignore_terms(args: &Value) -> Vec<String> {
    args.get("ignore_terms")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Parse the optional `glossary` object (35.9).  Shape:
/// `{ "banned": [...], "preferred": [...], "proper_nouns": [...] }`.
/// Each field is optional.  Missing object → empty glossary.
fn parse_glossary(args: &Value) -> crate::rules::glossary::ProjectGlossary {
    fn array_of_strings(v: Option<&Value>) -> Vec<String> {
        v.and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }
    let Some(glossary) = args.get("glossary").and_then(|v| v.as_object()) else {
        return crate::rules::glossary::ProjectGlossary::default();
    };
    crate::rules::glossary::ProjectGlossary {
        banned: array_of_strings(glossary.get("banned")),
        preferred: array_of_strings(glossary.get("preferred")),
        proper_nouns: array_of_strings(glossary.get("proper_nouns")),
    }
}

/// Remove political_coloring issues that the given stance suppresses.
fn filter_by_stance(issues: &mut Vec<Issue>, stance: PoliticalStance) {
    issues.retain(|issue| {
        issue.rule_type != IssueType::PoliticalColoring || stance.allows_rule(&issue.found)
    });
}

/// Downgrade issues whose found term matches a pre-built ignore set to Info.
fn apply_ignore_set(issues: &mut [Issue], ignore_set: &std::collections::HashSet<&str>) {
    if ignore_set.is_empty() {
        return;
    }
    for issue in issues {
        if ignore_set.contains(issue.found.as_str()) {
            issue.severity = Severity::Info;
        }
    }
}

/// Issue severity summary counts.
#[derive(Serialize)]
struct IssueSummary {
    errors: usize,
    warnings: usize,
    info: usize,
    /// Number of issues downgraded to Info by translation memory.
    /// Omitted (0) when TM is inactive or had no effect.
    #[serde(skip_serializing_if = "is_zero")]
    tm_suppressed: usize,
    /// Issues resolved by Tier 2 local disambiguation (context clues,
    /// profile priors, collocations).  Omitted (0) when Tier 2 had no effect.
    #[serde(skip_serializing_if = "is_zero")]
    tier2_resolved: usize,
    /// Issues in Tier 2 gray zone (forwarded to Tier 3 LLM).
    #[serde(skip_serializing_if = "is_zero")]
    tier2_gray_zone: usize,
    /// Number of sampling calls made during this invocation.
    /// Omitted (0) when sampling is inactive or unused.
    #[serde(skip_serializing_if = "is_zero")]
    sampling_used: usize,
    /// Number of eligible issues skipped because the sampling budget was exhausted.
    /// Omitted (0) when budget was not exhausted.
    #[serde(skip_serializing_if = "is_zero")]
    sampling_skipped: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Resolution tier counts and confidence distribution for the session.
/// Included in tool output when `include_stats` is true.
#[derive(Serialize)]
struct SummaryMetrics {
    deterministic_fixes: usize,
    heuristic_fixes: usize,
    llm_judged_fixes: usize,
    unresolved: usize,
    llm_calls: usize,
    llm_tokens: u64,
    confidence_distribution: ConfidenceDistribution,
}

/// Confidence buckets: high (deterministic + heuristic), medium (llm_judged),
/// low (unresolved).
#[derive(Serialize)]
struct ConfidenceDistribution {
    high: usize,
    medium: usize,
    low: usize,
}

/// Build summary_metrics from issues and accumulated stats.
fn build_summary_metrics(
    issues: &[Issue],
    sampling_stats: &SamplingStats,
    telemetry: Option<&TelemetryMetrics>,
) -> SummaryMetrics {
    let mut deterministic = 0usize;
    let mut heuristic = 0usize;
    let mut llm_judged = 0usize;
    let mut unresolved = 0usize;

    for issue in issues {
        match ResolutionTier::classify(issue) {
            ResolutionTier::Deterministic => deterministic += 1,
            ResolutionTier::Heuristic => heuristic += 1,
            ResolutionTier::LlmJudged => llm_judged += 1,
            ResolutionTier::Unresolved => unresolved += 1,
        }
    }

    let llm_tokens = telemetry.map_or(0, |t| {
        t.raw
            .estimated_prompt_tokens
            .saturating_add(t.raw.estimated_completion_tokens)
    });

    SummaryMetrics {
        deterministic_fixes: deterministic,
        heuristic_fixes: heuristic,
        llm_judged_fixes: llm_judged,
        unresolved,
        llm_calls: sampling_stats.used,
        llm_tokens,
        confidence_distribution: ConfidenceDistribution {
            high: deterministic + heuristic,
            medium: llm_judged,
            low: unresolved,
        },
    }
}

/// Gate status in the tool response.
#[derive(Serialize)]
struct GateInfo {
    enabled: bool,
    max_errors: usize,
    residual_errors: usize,
    max_warnings: usize,
    residual_warnings: usize,
}

/// Anchor provenance for explain mode (borrowed).
#[derive(Serialize)]
struct AnchorProvenance<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    anchor_en: Option<&'a str>,
    anchor_match: Option<bool>,
}

// `EditorialConfidence` is canonical-defined in `crate::rules::ruleset`
// so that `SpellingRule.editorial_confidence` and the per-issue field
// share a single type.  Re-exported here for the explain pipeline.
use crate::rules::ruleset::EditorialConfidence;

/// Structured per-issue explain metadata (35.2).
///
/// Surfaced only when `explain` is requested.  Helps reviewers understand
/// the confidence behind each suggestion without parsing free-form prose.
#[derive(Serialize)]
struct ExplainMeta<'a> {
    /// Why this is flagged.  Sourced from rule context + MoE refs when
    /// available; falls back to a structured restatement of the
    /// suggestion target.
    rationale: String,
    /// Domain that triggered the rule.  Parsed from `@domain X` markers
    /// in the rule's context field; defaults to "general".
    #[serde(skip_serializing_if = "Option::is_none")]
    domain: Option<&'a str>,
    /// True when the surface form is identical across zh-CN and zh-TW
    /// but the meaning differs (e.g. 文件: document vs file).
    is_false_friend: bool,
    /// Whether `--fix` would safely apply this suggestion.
    auto_fix_safe: bool,
    /// Whether the suggestion benefits from manual review.
    needs_review: bool,
    /// Per-issue editorial confidence — distinguishes binary corrections
    /// from style preferences (e.g. 優化, 算法 are valid zh-TW general
    /// vocabulary; 演算法 is the canonical computing form).
    editorial_confidence: EditorialConfidence,
}

/// Heuristic fallback when an issue lacks a rule-level
/// `editorial_confidence`.  Translationese / AI-style / grammar hits and
/// any `Info`-severity or anchor-rejected issue are surfaced as `Low`;
/// hits with explicit context support climb to `Medium`; everything else
/// is `High`.
fn heuristic_editorial_confidence(issue: &Issue) -> EditorialConfidence {
    use crate::rules::ruleset::{IssueType, Severity};

    let always_low = matches!(
        issue.rule_type,
        IssueType::Translationese | IssueType::AiStyle | IssueType::Grammar
    ) || issue.severity == Severity::Info
        || issue.anchor_match == Some(false);
    if always_low {
        return EditorialConfidence::Low;
    }
    if issue.context_clues.is_some() || issue.anchor_match == Some(true) {
        EditorialConfidence::Medium
    } else {
        EditorialConfidence::High
    }
}

/// Derive structured explain metadata for an issue.
///
/// Confidence resolution order:
///   1. Honor `issue.editorial_confidence` if the rule annotated it
///      (set in `assets/ruleset.json` per-rule).
///   2. Otherwise, fall back to heuristics on rule type / severity /
///      anchor_match / context_clues.
///
/// Invariants: `editorial_confidence == Low` ⇒ `auto_fix_safe = false`
/// AND `needs_review = true`.
fn derive_explain_meta(issue: &Issue) -> ExplainMeta<'_> {
    use crate::rules::ruleset::IssueType;

    // -- Domain extraction from `@domain X` markers in the rule context.
    let domain = issue.context.as_deref().and_then(|c| {
        let needle = "@domain ";
        c.find(needle).map(|i| {
            let rest = &c[i + needle.len()..];
            // Take up to the first whitespace, full-width comma, or period.
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '\u{FF0C}' || c == '\u{3002}')
                .unwrap_or(rest.len());
            rest[..end].trim()
        })
    });

    // -- Editorial confidence.
    // Rule-level annotation wins (from assets/ruleset.json
    // `editorial_confidence`); else heuristics on rule type / severity /
    // anchor_match / context_clues.
    let editorial_confidence = issue
        .editorial_confidence
        .unwrap_or_else(|| heuristic_editorial_confidence(issue));

    // -- False-friend detection.
    // Confusable rules are the canonical false friends.  Rule-tagged
    // low-confidence terms are also surfaced as false friends because
    // their surface form is shared across regions with divergent senses.
    let is_false_friend = matches!(issue.rule_type, IssueType::Confusable)
        || matches!(editorial_confidence, EditorialConfidence::Low)
            && issue.editorial_confidence.is_some();

    // -- Auto-fix safety + review need.
    // Invariant: `low` confidence forces auto_fix_safe=false +
    // needs_review=true.  Otherwise punctuation / case / variant / typo
    // hits with a single suggestion are auto-fix safe.
    let single_unambiguous = issue.suggestions.len() == 1
        && matches!(
            issue.rule_type,
            IssueType::Punctuation | IssueType::Case | IssueType::Variant | IssueType::Typo
        );

    let auto_fix_safe =
        !matches!(editorial_confidence, EditorialConfidence::Low) && single_unambiguous;

    let needs_review = matches!(editorial_confidence, EditorialConfidence::Low)
        || issue.suggestions.len() > 1
        || matches!(
            issue.rule_type,
            IssueType::Translationese | IssueType::AiStyle | IssueType::Grammar
        );

    let rationale = build_explanation(issue)
        .unwrap_or_else(|| format!("'{}' flagged by {:?} rule.", issue.found, issue.rule_type));

    ExplainMeta {
        rationale,
        domain,
        is_false_friend,
        auto_fix_safe,
        needs_review,
        editorial_confidence,
    }
}

/// Anchor provenance for compact mode (owned).
#[derive(Serialize)]
struct AnchorProvenanceOwned {
    #[serde(skip_serializing_if = "Option::is_none")]
    anchor_en: Option<String>,
    anchor_match: Option<bool>,
}

/// Issue with optional explain/stats annotations, serialized directly without
/// intermediate Value allocation.
#[derive(Serialize)]
struct AnnotatedIssue<'a> {
    #[serde(flatten)]
    issue: &'a Issue,
    #[serde(skip_serializing_if = "Option::is_none")]
    explanation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    anchor_provenance: Option<AnchorProvenance<'a>>,
    /// Structured per-issue explain metadata (35.2).  Present only in
    /// explain mode.  Carries domain, false-friend flag, auto-fix
    /// safety, review burden, and editorial confidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    explain_meta: Option<ExplainMeta<'a>>,
    /// Resolution tier: which pipeline stage authored this issue's resolution.
    /// Present only when `include_stats` is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    resolution: Option<ResolutionTier>,
}

/// Issues list: either plain references or annotated wrappers.
#[derive(Serialize)]
#[serde(untagged)]
enum IssuesList<'a> {
    Plain(&'a [Issue]),
    Annotated(Vec<AnnotatedIssue<'a>>),
}

/// Location in compact mode.
#[derive(Serialize)]
struct CompactLocation {
    line: usize,
    col: usize,
}

/// Calibration stats from translation verification.
#[cfg(feature = "translate")]
#[derive(Serialize)]
struct VerifyStats {
    api_ok: bool,
    matched: usize,
    unmatched: usize,
    no_english: usize,
}

/// Full-detail tool response (serialized directly, no intermediate Value).
#[derive(Serialize)]
struct FullOutput<'a> {
    accepted: bool,
    text: &'a str,
    issues: IssuesList<'a>,
    applied_fixes: usize,
    summary: &'a IssueSummary,
    gate: GateInfo,
    profile: &'a str,
    political_stance: &'a str,
    detected_script: &'a str,
    s2t_applied: bool,
    trace: &'a Trace,
    /// Present when fix_output != "full": indicates the `text` field contains
    /// a diff representation (search_replace blocks or patch JSON) instead of
    /// the full corrected text.
    #[serde(skip_serializing_if = "Option::is_none")]
    fix_output_mode: Option<&'a str>,
    #[cfg(feature = "translate")]
    #[serde(skip_serializing_if = "Option::is_none")]
    verify: Option<VerifyStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    coverage: Option<&'a crate::engine::scan::CoverageReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    oral_density: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    quality_flags: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_signature: Option<&'a crate::engine::ai_score::AiSignatureReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    translationese_signature: Option<&'a crate::engine::translationese_score::TranslationeseReport>,
    /// Composite three-axis style scorecard.  Present when the caller
    /// opts in via `detect_style: true` (the MCP equivalent of the CLI
    /// `--detect-style` shorthand).
    #[serde(skip_serializing_if = "Option::is_none")]
    style_scorecard: Option<&'a crate::engine::style_score::StyleScorecard>,
    #[serde(skip_serializing_if = "Option::is_none")]
    telemetry: Option<&'a TelemetryMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary_metrics: Option<&'a SummaryMetrics>,
    /// Document-wide consistency report (35.1).  Present only when the
    /// caller passed `consistency: true` AND mixed regional usage
    /// (both `線程` and `執行緒`, etc.) is detected in the document.
    #[serde(skip_serializing_if = "Option::is_none")]
    consistency: Option<&'a crate::engine::consistency::ConsistencyReport>,
}

/// Compact tool response (serialized directly, no intermediate Value).
#[derive(Serialize)]
struct CompactOutput<'a> {
    accepted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<&'a str>,
    issues: Vec<CompactGroup>,
    applied_fixes: usize,
    summary: &'a IssueSummary,
    gate: GateInfo,
    profile: &'a str,
    detected_script: &'a str,
    s2t_applied: bool,
    /// Present when fix_output != "full": indicates the `text` field contains
    /// a diff representation instead of the full corrected text.
    #[serde(skip_serializing_if = "Option::is_none")]
    fix_output_mode: Option<&'a str>,
    #[cfg(feature = "translate")]
    #[serde(skip_serializing_if = "Option::is_none")]
    verify: Option<VerifyStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    coverage: Option<&'a crate::engine::scan::CoverageReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    oral_density: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    quality_flags: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_signature: Option<&'a crate::engine::ai_score::AiSignatureReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    translationese_signature: Option<&'a crate::engine::translationese_score::TranslationeseReport>,
    /// Composite three-axis style scorecard.  Present when the caller
    /// opts in via `detect_style: true` (the MCP equivalent of the CLI
    /// `--detect-style` shorthand).
    #[serde(skip_serializing_if = "Option::is_none")]
    style_scorecard: Option<&'a crate::engine::style_score::StyleScorecard>,
    #[serde(skip_serializing_if = "Option::is_none")]
    telemetry: Option<&'a TelemetryMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary_metrics: Option<&'a SummaryMetrics>,
}

/// Summary-only output: issue counts + AI signature, no individual issues or text.
#[derive(Serialize)]
struct SummaryOutput<'a> {
    accepted: bool,
    summary: &'a IssueSummary,
    gate: GateInfo,
    profile: &'a str,
    detected_script: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    coverage: Option<&'a crate::engine::scan::CoverageReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    oral_density: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    quality_flags: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_signature: Option<&'a crate::engine::ai_score::AiSignatureReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    translationese_signature: Option<&'a crate::engine::translationese_score::TranslationeseReport>,
    /// Composite three-axis style scorecard.  Present when the caller
    /// opts in via `detect_style: true` (the MCP equivalent of the CLI
    /// `--detect-style` shorthand).
    #[serde(skip_serializing_if = "Option::is_none")]
    style_scorecard: Option<&'a crate::engine::style_score::StyleScorecard>,
    #[serde(skip_serializing_if = "Option::is_none")]
    telemetry: Option<&'a TelemetryMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary_metrics: Option<&'a SummaryMetrics>,
}

/// Count issues by severity.
fn build_summary(
    issues: &[Issue],
    tm_suppressed: usize,
    sampling_stats: SamplingStats,
    disambig_stats: &DisambigStats,
) -> IssueSummary {
    let mut s = IssueSummary {
        errors: 0,
        warnings: 0,
        info: 0,
        tm_suppressed,
        tier2_resolved: disambig_stats.tier2_resolved,
        tier2_gray_zone: disambig_stats.gray_zone,
        sampling_used: sampling_stats.used,
        sampling_skipped: sampling_stats.skipped,
    };
    for issue in issues {
        match issue.severity {
            Severity::Error => s.errors += 1,
            Severity::Warning => s.warnings += 1,
            Severity::Info => s.info += 1,
        }
    }
    s
}

/// Parameters for build_check_output.
struct CheckOutputParams<'a> {
    result_text: &'a str,
    issues: &'a [Issue],
    applied_fixes: usize,
    max_errors: Option<u64>,
    max_warnings: Option<u64>,
    profile: Profile,
    stance_name: &'a str,
    detected_script: &'a str,
    /// Whether S2T conversion was applied (input was Simplified Chinese).
    s2t_applied: bool,
    trace: &'a Trace,
    explain: bool,
    output_mode: OutputMode,
    has_fixes: bool,
    /// Fix output mode: full text, search/replace blocks, or patch array.
    fix_output: FixOutputMode,
    /// Original text before fixes (needed for search_replace and patch modes).
    original_text: &'a str,
    /// Applied fix records for patch/search_replace output.
    fix_records: &'a [crate::fixer::AppliedFix],
    #[cfg(feature = "translate")]
    calibrate_result: Option<crate::engine::translate::CalibrateResult>,
    coverage: Option<&'a crate::engine::scan::CoverageReport>,
    oral_density: Option<f32>,
    quality_flags: &'a [String],
    ai_signature: Option<&'a crate::engine::ai_score::AiSignatureReport>,
    translationese_signature: Option<&'a crate::engine::translationese_score::TranslationeseReport>,
    /// Composite style scorecard (or `None` when not requested).
    /// The caller computes this once per scan; build_check_output forwards
    /// it untouched into the chosen output mode.
    style_scorecard: Option<&'a crate::engine::style_score::StyleScorecard>,
    /// Number of issues downgraded by translation memory.
    tm_suppressed: usize,
    /// Sampling budget usage statistics.
    sampling_stats: SamplingStats,
    /// Tier 2 disambiguation statistics.
    disambig_stats: DisambigStats,
    /// Token telemetry metrics (only when include_telemetry is true).
    telemetry: Option<TelemetryMetrics>,
    /// Whether to include per-issue resolution tier and summary_metrics.
    include_stats: bool,
    /// Document-wide consistency report (35.1).  Some only when the
    /// caller requested `consistency: true` AND mixed regional usage
    /// is detected.
    consistency: Option<&'a crate::engine::consistency::ConsistencyReport>,
}

/// Build telemetry metrics from accumulated counters.
/// `cache_counts` is (hits, misses) from the judgment cache.
fn build_telemetry(
    text: &str,
    scanner_hit_count: usize,
    disambig_stats: &DisambigStats,
    sampling_stats: &SamplingStats,
    bridge: Option<&&mut SamplingBridge<'_>>,
    applied_fixes: usize,
    cache_counts: (u64, u64),
) -> TelemetryMetrics {
    let (est_prompt_tokens, est_completion_tokens) = bridge
        .map(|b| (b.est_prompt_tokens, b.est_completion_tokens))
        .unwrap_or((0, 0));
    // ambiguous_terms: all terms that entered Tier 2 evaluation
    // (resolved + suppressed + gray_zone), not just those forwarded to Tier 3.
    let ambiguous_terms = (disambig_stats.tier2_resolved
        + disambig_stats.suppressed
        + disambig_stats.gray_zone) as u64;
    let t = TokenTelemetry {
        input_chars: text.chars().count() as u64,
        rule_hits: scanner_hit_count as u64,
        ambiguous_terms,
        tier2_resolved: disambig_stats.tier2_resolved as u64,
        llm_round_trips: sampling_stats.used as u64,
        final_fixes: applied_fixes as u64,
        prompt_tokens: est_prompt_tokens,
        completion_tokens: est_completion_tokens,
        cache_hits: cache_counts.0,
        cache_misses: cache_counts.1,
    };
    t.derive_metrics()
}

/// Build the unified zhtw JSON response and wrap it in a CallToolResult.
///
/// Both the lint-only and fix paths produce the same output shape; only the
/// concrete values differ. Compact mode omits text (in lint-only), trace,
/// byte offsets/lengths, and deduplicates repeated issues.
///
/// Serializes typed structs directly to avoid intermediate `serde_json::Value`
/// allocations. Uses compact JSON by default; set `ZHTW_PRETTY=1` env var
/// for indented output during debugging.
/// Build the composite three-axis scorecard when the caller explicitly
/// opts in via `detect_style` (CLI: `--detect-style` flag, MCP:
/// `detect_style: true` argument).  Pure aggregation — returns `None`
/// when not requested so the standard payload stays lean.
fn style_scorecard_for(
    detect_style: bool,
    ai: Option<&crate::engine::ai_score::AiSignatureReport>,
    trans: Option<&crate::engine::translationese_score::TranslationeseReport>,
    issues: &[Issue],
    text: &str,
) -> Option<crate::engine::style_score::StyleScorecard> {
    if !detect_style {
        return None;
    }
    Some(crate::engine::style_score::StyleScorecard::build(
        ai,
        trans,
        issues,
        text.chars().count(),
    ))
}

fn build_check_output(params: &CheckOutputParams<'_>) -> CallToolResult {
    let summary = build_summary(
        params.issues,
        params.tm_suppressed,
        params.sampling_stats,
        &params.disambig_stats,
    );

    let stats_metrics = if params.include_stats {
        Some(build_summary_metrics(
            params.issues,
            &params.sampling_stats,
            params.telemetry.as_ref(),
        ))
    } else {
        None
    };

    let max_err = params.max_errors.unwrap_or(0) as usize;
    let max_warn = params.max_warnings.unwrap_or(0) as usize;
    let gate_enabled = params.max_errors.is_some() || params.max_warnings.is_some();
    let accepted = params.max_errors.is_none_or(|_| summary.errors <= max_err)
        && params
            .max_warnings
            .is_none_or(|_| summary.warnings <= max_warn);

    let gate = GateInfo {
        enabled: gate_enabled,
        max_errors: max_err,
        residual_errors: summary.errors,
        max_warnings: max_warn,
        residual_warnings: summary.warnings,
    };

    #[cfg(feature = "translate")]
    let verify = params.calibrate_result.as_ref().map(|cr| VerifyStats {
        api_ok: cr.api_ok,
        matched: cr.matched,
        unmatched: cr.unmatched,
        no_english: cr.no_english,
    });

    // When fix_output is not Full and fixes were applied, replace the text
    // field with a diff representation to save output tokens.
    let diff_text: Option<String> = if params.has_fixes
        && params.fix_output != FixOutputMode::Full
        && !params.fix_records.is_empty()
    {
        Some(build_fix_diff(
            params.original_text,
            params.fix_records,
            params.fix_output,
        ))
    } else {
        None
    };
    let effective_text = diff_text.as_deref().unwrap_or(params.result_text);

    let fix_mode_label = if diff_text.is_some() {
        Some(params.fix_output.name())
    } else {
        None
    };
    let quality_flags = (!params.quality_flags.is_empty()).then_some(params.quality_flags);

    let serialize_result = match params.output_mode {
        OutputMode::Full => {
            let issues = build_issues_list(params.issues, params.explain, params.include_stats);
            let output = FullOutput {
                accepted,
                text: effective_text,
                issues,
                applied_fixes: params.applied_fixes,
                summary: &summary,
                gate,
                profile: params.profile.name(),
                political_stance: params.stance_name,
                detected_script: params.detected_script,
                s2t_applied: params.s2t_applied,
                trace: params.trace,
                fix_output_mode: fix_mode_label,
                #[cfg(feature = "translate")]
                verify,
                coverage: params.coverage,
                oral_density: params.oral_density,
                quality_flags,
                ai_signature: params.ai_signature,
                translationese_signature: params.translationese_signature,
                style_scorecard: params.style_scorecard,
                telemetry: params.telemetry.as_ref(),
                summary_metrics: stats_metrics.as_ref(),
                consistency: params.consistency,
            };
            serialize_output(&output)
        }
        OutputMode::Compact => {
            let issues = build_compact_groups(params.issues, params.explain, params.include_stats);
            let output = CompactOutput {
                accepted,
                text: if params.has_fixes {
                    Some(effective_text)
                } else {
                    None
                },
                issues,
                applied_fixes: params.applied_fixes,
                summary: &summary,
                gate,
                profile: params.profile.name(),
                detected_script: params.detected_script,
                s2t_applied: params.s2t_applied,
                fix_output_mode: fix_mode_label,
                #[cfg(feature = "translate")]
                verify,
                coverage: params.coverage,
                oral_density: params.oral_density,
                quality_flags,
                ai_signature: params.ai_signature,
                translationese_signature: params.translationese_signature,
                style_scorecard: params.style_scorecard,
                telemetry: params.telemetry.as_ref(),
                summary_metrics: stats_metrics.as_ref(),
            };
            serialize_output(&output)
        }
        OutputMode::Tabular => {
            let tsv = build_tabular_output(
                accepted,
                params.issues,
                params.applied_fixes,
                &summary,
                params.has_fixes,
                effective_text,
                params.explain,
                fix_mode_label,
            );
            Ok(tsv)
        }
        OutputMode::Summary => {
            let output = SummaryOutput {
                accepted,
                summary: &summary,
                gate,
                profile: params.profile.name(),
                detected_script: params.detected_script,
                coverage: params.coverage,
                oral_density: params.oral_density,
                quality_flags,
                ai_signature: params.ai_signature,
                translationese_signature: params.translationese_signature,
                style_scorecard: params.style_scorecard,
                telemetry: params.telemetry.as_ref(),
                summary_metrics: stats_metrics.as_ref(),
            };
            serialize_output(&output)
        }
    };

    match serialize_result {
        Ok(json_str) => {
            if accepted {
                CallToolResult::text(json_str)
            } else {
                CallToolResult::error(json_str)
            }
        }
        Err(e) => {
            tracing::error!("failed to serialize check output: {e}");
            CallToolResult::error("internal server error".into())
        }
    }
}

/// Serialize to compact JSON by default; pretty-print when `ZHTW_PRETTY=1`.
fn serialize_output(output: &impl serde::Serialize) -> serde_json::Result<String> {
    if std::env::var_os("ZHTW_PRETTY").is_some_and(|v| v == "1") {
        serde_json::to_string_pretty(output)
    } else {
        serde_json::to_string(output)
    }
}

/// Build issues list for full output mode: either plain references (no extra
/// fields) or annotated wrappers with explanation, anchor provenance, and/or
/// resolution tier.
fn build_issues_list<'a>(
    issues: &'a [Issue],
    explain: bool,
    include_stats: bool,
) -> IssuesList<'a> {
    if explain || include_stats {
        let annotated: Vec<AnnotatedIssue<'a>> = issues
            .iter()
            .map(|issue| {
                let explanation = if explain {
                    build_explanation(issue)
                } else {
                    None
                };
                let anchor_provenance = if explain && issue.anchor_match.is_some() {
                    Some(AnchorProvenance {
                        anchor_en: issue.english.as_deref(),
                        anchor_match: issue.anchor_match,
                    })
                } else {
                    None
                };
                let resolution = if include_stats {
                    Some(ResolutionTier::classify(issue))
                } else {
                    None
                };
                let explain_meta = if explain {
                    Some(derive_explain_meta(issue))
                } else {
                    None
                };
                AnnotatedIssue {
                    issue,
                    explanation,
                    anchor_provenance,
                    explain_meta,
                    resolution,
                }
            })
            .collect();
        IssuesList::Annotated(annotated)
    } else {
        IssuesList::Plain(issues)
    }
}

/// Build compact deduplicated issues array.
///
/// Groups issues by (found, rule_type, suggestions, severity) key. Each group
/// becomes one entry with count and locations. Serialized directly via
/// `#[derive(Serialize)]` on `CompactGroup` — no intermediate `Value` per group.
fn build_compact_groups(issues: &[Issue], explain: bool, include_stats: bool) -> Vec<CompactGroup> {
    use std::collections::BTreeMap;

    // Key: (found, rule_type, suggestions_joined, severity, resolution_tier_discriminant)
    // Include severity so that sampling can produce mixed-severity occurrences
    // of the same term without silently inheriting the first occurrence's level.
    // When include_stats is true, also partition by resolution tier so the
    // per-group resolution field is accurate.
    // Uses shared IssueType::name() and Severity::name() from ruleset.rs.
    // We use BTreeMap for deterministic ordering.
    let mut groups: BTreeMap<(&str, &str, String, &str, u8), CompactGroup> = BTreeMap::new();

    for issue in issues {
        let rt = issue.rule_type.name();
        let sug_key = issue.suggestions.join("|");
        let sev_key = issue.severity.name();
        // Compute resolution tier once; reuse for both grouping key and field value.
        // Discriminant 0 when stats disabled (all group together); distinct
        // per-tier when enabled so the resolution field stays accurate.
        let tier = if include_stats {
            Some(ResolutionTier::classify(issue))
        } else {
            None
        };
        let tier_disc = tier.map_or(0, |t| t as u8 + 1);
        let key = (issue.found.as_str(), rt, sug_key, sev_key, tier_disc);

        let group = groups.entry(key).or_insert_with(|| CompactGroup {
            found: issue.found.clone(),
            suggestions: issue.suggestions.to_vec(),
            suggested_rewrite: issue.suggested_rewrite.clone(),
            rule_type: rt.to_string(),
            severity: issue.severity.name().to_string(),
            context: issue.context.as_deref().map(str::to_string),
            english: issue.english.as_deref().map(str::to_string),
            explanation: if explain {
                build_explanation(issue)
            } else {
                None
            },
            anchor_provenance: if explain && issue.anchor_match.is_some() {
                Some(AnchorProvenanceOwned {
                    anchor_en: issue.english.as_deref().map(str::to_string),
                    anchor_match: issue.anchor_match,
                })
            } else {
                None
            },
            resolution: tier,
            count: 0,
            locations: Vec::new(),
        });
        group.count += 1;
        group.locations.push(CompactLocation {
            line: issue.line,
            col: issue.col,
        });
    }

    groups.into_values().collect()
}

/// Escape tab, newline, and carriage return in a TSV field to prevent
/// column/row injection.  Returns a borrowed reference when no escaping
/// is needed, avoiding allocation on the common path.
pub fn escape_tsv_field(s: &str) -> std::borrow::Cow<'_, str> {
    if s.bytes()
        .any(|b| b == b'\\' || b == b'\t' || b == b'\n' || b == b'\r')
    {
        let mut out = String::with_capacity(s.len());
        for ch in s.chars() {
            match ch {
                '\\' => out.push_str("\\\\"),
                '\t' => out.push_str("\\t"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                _ => out.push(ch),
            }
        }
        std::borrow::Cow::Owned(out)
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

/// Deduplicated issue group shared by MCP tabular output and CLI tabular format.
///
/// Groups issues by (found, rule_type, suggestions, severity) key. Each group
/// stores shared fields once and collects per-occurrence locations.
pub struct IssueGroup {
    pub suggestions: Vec<String>,
    pub count: usize,
    pub locs: Vec<(usize, usize)>,
    pub explanation: Option<String>,
}

/// Issue grouping key: (found, rule_type, suggestions_joined, severity).
pub type IssueGroupKey<'a> = (&'a str, &'a str, String, &'a str);

/// Group issues by (found, rule_type, suggestions, severity) into a BTreeMap
/// for deterministic ordering. Optionally generates explanations per group.
pub fn group_issues<'a>(
    issues: &'a [Issue],
    explain: bool,
) -> std::collections::BTreeMap<IssueGroupKey<'a>, IssueGroup> {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<IssueGroupKey<'a>, IssueGroup> = BTreeMap::new();
    for issue in issues {
        let rt = issue.rule_type.name();
        let sug_key = issue.suggestions.join("|");
        let sev = issue.severity.name();
        let key: IssueGroupKey<'a> = (issue.found.as_str(), rt, sug_key, sev);
        let entry = groups.entry(key).or_insert_with(|| IssueGroup {
            suggestions: issue.suggestions.to_vec(),
            count: 0,
            locs: Vec::new(),
            explanation: if explain {
                build_explanation(issue)
            } else {
                None
            },
        });
        entry.count += 1;
        entry.locs.push((issue.line, issue.col));
    }
    groups
}

/// Map full severity name to single-letter code for tabular output.
pub fn shorten_severity(sev: &str) -> &str {
    match sev {
        "error" => "E",
        "warning" => "W",
        "info" => "I",
        _ => sev,
    }
}

/// Map full issue type name to abbreviated code for tabular output.
pub fn shorten_type(rt: &str) -> &str {
    match rt {
        "political_coloring" => "pol",
        "cross_strait" => "cs",
        "typo" => "typo",
        "confusable" => "cf",
        "case" => "case",
        "punctuation" => "punc",
        "variant" => "v",
        "grammar" => "gram",
        _ => rt,
    }
}

/// Compress a list of (line, col) locations into a compact string.
///
/// When all locations share the same column, emits "L1,L4,L7:C" instead of
/// the verbose "1:C,4:C,7:C" form -- saves tokens on repeated issues.
pub fn compress_locations(locs: &[(usize, usize)]) -> String {
    use std::fmt::Write;
    if locs.is_empty() {
        return String::new();
    }
    if locs.len() == 1 {
        return format!("{}:{}", locs[0].0, locs[0].1);
    }
    // Check if all columns are identical.
    let first_col = locs[0].1;
    if locs.iter().all(|(_, c)| *c == first_col) {
        let mut s = String::new();
        for (i, (line, _)) in locs.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            let _ = write!(s, "{line}");
        }
        let _ = write!(s, ":{first_col}");
        s
    } else {
        locs.iter()
            .map(|(l, c)| format!("{l}:{c}"))
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// Build header-once TSV output for LLM-facing responses.
///
/// Eliminates JSON syntax tax: no repeated keys, braces, or quotes per issue.
/// Header row defines column semantics; data rows are tab-separated.
/// Achieves >=50% token reduction vs compact JSON on typical responses.
#[allow(clippy::too_many_arguments)]
fn build_tabular_output(
    accepted: bool,
    issues: &[Issue],
    applied_fixes: usize,
    summary: &IssueSummary,
    has_fixes: bool,
    result_text: &str,
    explain: bool,
    fix_output_mode: Option<&str>,
) -> String {
    use std::fmt::Write;

    let mut out = String::with_capacity(256);

    // Meta line: key=value pairs, omitting zero-count fields to save tokens.
    let _ = write!(out, "#ok={}", accepted);
    if summary.errors > 0 {
        let _ = write!(out, "\terr={}", summary.errors);
    }
    if summary.warnings > 0 {
        let _ = write!(out, "\twarn={}", summary.warnings);
    }
    if summary.info > 0 {
        let _ = write!(out, "\tinfo={}", summary.info);
    }
    if applied_fixes > 0 {
        let _ = write!(out, "\tfix={}", applied_fixes);
    }
    if has_fixes {
        let _ = write!(out, "\ttxt={}", result_text.len());
    }
    if let Some(mode) = fix_output_mode {
        let _ = write!(out, "\tfix_fmt={mode}");
    }
    out.push('\n');

    let groups = group_issues(issues, explain);

    // Header row.
    if explain {
        out.push_str("found\tsug\ttype\tsev\tn\tloc\texpl\n");
    } else {
        out.push_str("found\tsug\ttype\tsev\tn\tloc\n");
    }

    // Data rows.  Use abbreviated severity (E/W/I) and rule type codes
    // (cs/cf/v/pol/typo/punc/case/gram) to reduce token count.
    // Escape tab/newline in data fields to prevent TSV injection.
    for ((found, rt, _, sev), group) in &groups {
        let found_safe = escape_tsv_field(found);
        let suggestions_str = group
            .suggestions
            .iter()
            .map(|s| escape_tsv_field(s))
            .collect::<Vec<_>>()
            .join(",");

        // Map full group-key names to abbreviated codes directly,
        // avoiding an O(groups*issues) scan that could also mismatch
        // when the same found term appears in multiple groups.
        let short_rt = shorten_type(rt);
        let short_sev = shorten_severity(sev);

        // Compress locations: if all share the same column, emit
        // "L1,L4,L7:C" instead of "L1:C,L4:C,L7:C".
        let locs_str = compress_locations(&group.locs);

        let _ = write!(
            out,
            "{found_safe}\t{suggestions_str}\t{short_rt}\t{short_sev}\t{}\t{locs_str}",
            group.count,
        );
        if explain {
            out.push('\t');
            if let Some(expl) = &group.explanation {
                out.push_str(&escape_tsv_field(expl));
            }
        }
        out.push('\n');
    }

    // If fixes were applied, append the fixed text after a separator.
    if has_fixes {
        out.push_str("#text\n");
        out.push_str(result_text);
    }

    out
}

/// Build diff representation of fixes for token-efficient output.
///
/// For SearchReplace mode: emits <<<<<<< SEARCH / ======= REPLACE / >>>>>>> END
/// blocks that LLMs can parse reliably without byte arithmetic.
/// For Patch mode: emits a JSON patches array with byte offsets, sorted
/// descending by offset so clients can apply in order without index shifting.
fn build_fix_diff(
    original_text: &str,
    fix_records: &[crate::fixer::AppliedFix],
    mode: FixOutputMode,
) -> String {
    match mode {
        FixOutputMode::SearchReplace => {
            let mut out = String::with_capacity(fix_records.len() * 80);
            for fix in fix_records {
                // Safe slice: get() returns None if offset/end are out of
                // bounds or not on UTF-8 char boundaries.
                if let Some(found) = original_text.get(fix.offset..fix.offset + fix.old_len) {
                    out.push_str("<<<<<<< SEARCH\n");
                    out.push_str(found);
                    out.push_str("\n======= REPLACE\n");
                    out.push_str(&fix.replacement);
                    out.push_str("\n>>>>>>> END\n");
                }
            }
            out
        }
        FixOutputMode::Patch => {
            use std::fmt::Write;
            // TSV patch format: header-once, sorted descending by offset so
            // clients can apply in order without index shifting.
            let mut patches: Vec<(usize, usize, &str, &str)> = fix_records
                .iter()
                .filter_map(|fix| {
                    let found = original_text.get(fix.offset..fix.offset + fix.old_len)?;
                    Some((fix.offset, fix.old_len, found, fix.replacement.as_str()))
                })
                .collect();
            patches.sort_by_key(|p| std::cmp::Reverse(p.0));

            let mut out = String::with_capacity(patches.len() * 40);
            let _ = writeln!(out, "#patches={}", patches.len());
            out.push_str("offset\tlength\tfound\treplacement\n");
            for (offset, length, found, replacement) in &patches {
                let _ = writeln!(
                    out,
                    "{offset}\t{length}\t{}\t{}",
                    escape_tsv_field(found),
                    escape_tsv_field(replacement),
                );
            }
            out
        }
        FixOutputMode::Full => {
            // Should never reach here; caller guards.
            String::new()
        }
    }
}

/// Helper for compact mode issue grouping.
#[derive(Serialize)]
struct CompactGroup {
    found: String,
    suggestions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    suggested_rewrite: Option<String>,
    rule_type: String,
    severity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    english: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    explanation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    anchor_provenance: Option<AnchorProvenanceOwned>,
    /// Resolution tier for all issues in this group.
    #[serde(skip_serializing_if = "Option::is_none")]
    resolution: Option<ResolutionTier>,
    count: usize,
    locations: Vec<CompactLocation>,
}

// Tool definitions (JSON Schema for zhtw)

fn tool_definitions() -> Vec<ToolDef> {
    vec![ToolDef {
        name: "zhtw".into(),
        description: "Lint/fix/gate zh-TW text. Auto-converts Simplified Chinese to Traditional before applying rules. Use verify=true to calibrate issues via Google Translate anchor matching.".into(),
        input_schema: {
            let mut props = serde_json::Map::new();
            props.insert("text".into(), json!({ "type": "string" }));
            props.insert("fix_mode".into(), json!({
                "type": "string",
                "enum": ["none", "orthographic", "lexical_safe", "lexical_contextual"]
            }));
            props.insert("max_errors".into(), json!({ "type": "integer" }));
            props.insert("max_warnings".into(), json!({ "type": "integer" }));
            props.insert("profile".into(), json!({
                "type": "string",
                "enum": ["base", "strict"],
                "description": "Norm strictness: 'base' (default) or 'strict' (full MoE with character variants)"
            }));
            props.insert("relaxed".into(), json!({
                "type": "boolean",
                "description": "Capability flag for software UI strings: disables colon enforcement, dunhao detection, grammar checks; uses en-dash for ranges"
            }));
            props.insert("exempt_blockquotes".into(), json!({
                "type": "boolean",
                "description": "Markdown only: exclude pulldown-cmark `Tag::BlockQuote` ranges from scanning.  Useful when a document quotes mainland-Chinese sources for illustrative purposes.  Off by default."
            }));
            props.insert("content_type".into(), json!({
                "type": "string",
                "enum": ["plain", "markdown", "markdown-scan-code", "yaml"]
            }));
            props.insert("political_stance".into(), json!({
                "type": "string",
                "enum": ["roc_centric", "international", "neutral"]
            }));
            props.insert("ignore_terms".into(), json!({
                "type": "array",
                "items": { "type": "string" }
            }));
            props.insert("glossary".into(), json!({
                "type": "object",
                "description": "Project-level glossary.  `banned` terms always fire (project-wide truth, banned > TM); `proper_nouns` suppress matching issues; `preferred` chooses canonical TW form for the consistency report.",
                "properties": {
                    "banned": { "type": "array", "items": { "type": "string" } },
                    "preferred": { "type": "array", "items": { "type": "string" } },
                    "proper_nouns": { "type": "array", "items": { "type": "string" } },
                }
            }));
            props.insert("consistency".into(), json!({
                "type": "boolean",
                "description": "Emit a `consistency` block when both regional variants of one concept appear in the document (e.g. both 線程 and 執行緒).  Off by default."
            }));
            props.insert("explain".into(), json!({ "type": "boolean" }));
            props.insert("fix_output".into(), json!({
                "type": "string",
                "enum": ["full", "search_replace", "patch"],
                "description": "Fix output format: full text (default), search/replace blocks, or patch array with byte offsets"
            }));
            #[cfg(feature = "translate")]
            props.insert("verify".into(), json!({
                "type": "boolean",
                "description": "Anchor-verify issues via Google Translate"
            }));
            props.insert("output".into(), json!({
                "type": "string",
                "enum": ["full", "compact", "tabular", "summary"],
                "description": "Output mode. 'summary' returns only issue counts + AI signature (no individual issues)"
            }));
            props.insert("detect_ai".into(), json!({
                "type": "boolean",
                "description": "Enable AI writing artifact detection (density + grammar patterns). Default: on. Set false to suppress AI filler findings."
            }));
            props.insert("detect_translationese".into(), json!({
                "type": "boolean",
                "description": "Enable translationese (翻譯腔 / 歐化) detection — Europeanized syntax and calques from the dewesternise checklist. Default: on. Orthogonal to detect_ai; reported separately."
            }));
            props.insert("detect_style".into(), json!({
                "type": "boolean",
                "description": "Composite style scorecard: emit `style_scorecard` with three orthogonal axes (ai, translationese, consistency) plus top contributing issues. Default: false. Three scores never collapsed into a single number."
            }));
            props.insert("translationese_domain".into(), json!({
                "type": "string",
                "enum": ["general", "technical", "literary", "news"],
                "description": "Per-domain calibration for translationese scoring thresholds. 'technical' tolerates more passive voice and weak-verb nominalization; 'literary' is the strictest; 'news' favors active voice. Default: 'general'."
            }));
            props.insert("ai_threshold".into(), json!({
                "type": "string",
                "enum": ["low", "medium", "high"],
                "description": "AI detection sensitivity: 'low' (sensitive, catches more), 'medium' (balanced), 'high' (conservative). Only effective with detect_ai=true"
            }));
            props.insert("include_telemetry".into(), json!({
                "type": "boolean",
                "description": "Include per-request token telemetry metrics in the response (LLM cost accounting)"
            }));
            props.insert("include_stats".into(), json!({
                "type": "boolean",
                "description": "Include per-issue resolution tier and session-level summary_metrics (deterministic/heuristic/llm_judged/unresolved counts, confidence distribution)"
            }));
            json!({
                "type": "object",
                "properties": Value::Object(props),
                "required": ["text"]
            })
        },
        annotations: Some(ToolAnnotations {
            destructive: None,
            idempotent: Some(true),
            read_only: Some(true),
            open_world: None,
        }),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::types::RequestId;
    use crate::rules::ruleset::Tier2Outcome;

    /// Tool annotations must serialize with the MCP-spec `*Hint` wire names;
    /// any other spelling is silently dropped by spec-compliant clients.
    #[test]
    fn tool_annotations_use_spec_hint_wire_names() {
        let defs = serde_json::to_value(tool_definitions()).unwrap();
        let ann = &defs[0]["annotations"];
        assert_eq!(ann["idempotentHint"], true);
        assert_eq!(ann["readOnlyHint"], true);
        // Bare (non-Hint) spellings must not appear on the wire.
        assert!(ann.get("idempotent").is_none());
        assert!(ann.get("readOnly").is_none());
        assert!(ann.get("destructive").is_none());
    }

    /// 35.2 — high confidence: cross_strait without context_clues, single
    /// suggestion.  Auto-fix safety still gated on rule_type being one of
    /// the unambiguous classes (Punctuation/Case/Variant/Typo); a plain
    /// CrossStrait keeps `auto_fix_safe=false` because the choice between
    /// suggestions is editorial.
    #[test]
    fn explain_meta_high_confidence_for_unambiguous_cross_strait() {
        let mut issue = Issue::new(
            0,
            6,
            "線程",
            vec!["執行緒".into()],
            IssueType::CrossStrait,
            Severity::Warning,
        );
        issue.english = Some(std::sync::Arc::from("thread"));
        let meta = derive_explain_meta(&issue);
        assert!(matches!(
            meta.editorial_confidence,
            EditorialConfidence::High
        ));
        assert!(!meta.is_false_friend);
        assert!(!meta.needs_review);
    }

    /// 35.2 — rule-tagged low confidence (e.g. `優化`, `算法`, `場景`
    /// in `assets/ruleset.json`) surfaces as `low` so reviewers know
    /// they are editorial preference, not binary error.  Invariant:
    /// low ⇒ auto_fix_safe=false AND needs_review=true.
    #[test]
    fn explain_meta_low_confidence_for_rule_tagged_boundary_terms() {
        for boundary in &["優化", "算法", "場景"] {
            let mut issue = Issue::new(
                0,
                boundary.len(),
                *boundary,
                vec!["演算法".into()],
                IssueType::CrossStrait,
                Severity::Warning,
            );
            issue.editorial_confidence = Some(EditorialConfidence::Low);
            let meta = derive_explain_meta(&issue);
            assert!(
                matches!(meta.editorial_confidence, EditorialConfidence::Low),
                "rule-tagged {boundary} must be low confidence",
            );
            assert!(
                !meta.auto_fix_safe,
                "rule-tagged {boundary}: low ⇒ !auto_fix_safe"
            );
            assert!(
                meta.needs_review,
                "rule-tagged {boundary}: low ⇒ needs_review"
            );
            assert!(
                meta.is_false_friend,
                "rule-tagged {boundary}: marked false friend"
            );
        }
    }

    /// 35.2 — `@domain X` extraction populates the `domain` field.
    #[test]
    fn explain_meta_extracts_domain_from_context() {
        let mut issue = Issue::new(
            0,
            6,
            "用戶",
            vec!["使用者".into()],
            IssueType::CrossStrait,
            Severity::Warning,
        );
        issue.context = Some(std::sync::Arc::from("@domain IT。其他註解"));
        let meta = derive_explain_meta(&issue);
        assert_eq!(meta.domain, Some("IT"));
    }

    /// 35.2 — Translationese / AiStyle / Grammar always demand review.
    #[test]
    fn explain_meta_translationese_marks_low_confidence() {
        let issue = Issue::new(
            0,
            3,
            "被",
            vec!["主動句".into()],
            IssueType::Translationese,
            Severity::Info,
        );
        let meta = derive_explain_meta(&issue);
        assert!(matches!(
            meta.editorial_confidence,
            EditorialConfidence::Low
        ));
        assert!(!meta.auto_fix_safe);
        assert!(meta.needs_review);
    }

    /// 35.9 — `parse_glossary` extracts banned/preferred/proper_nouns
    /// from the tool args object.
    #[test]
    fn parse_glossary_extracts_three_lists() {
        let args = serde_json::json!({
            "glossary": {
                "banned": ["線程", "內存"],
                "preferred": ["執行緒"],
                "proper_nouns": ["TSMC"],
            }
        });
        let g = parse_glossary(&args);
        assert_eq!(g.banned, vec!["線程".to_string(), "內存".to_string()]);
        assert_eq!(g.preferred, vec!["執行緒".to_string()]);
        assert_eq!(g.proper_nouns, vec!["TSMC".to_string()]);
    }

    #[test]
    fn parse_glossary_missing_object_returns_empty() {
        let args = serde_json::json!({});
        let g = parse_glossary(&args);
        assert!(g.is_empty());
    }

    #[test]
    fn parse_glossary_partial_fields_default_to_empty() {
        let args = serde_json::json!({"glossary": {"banned": ["X"]}});
        let g = parse_glossary(&args);
        assert_eq!(g.banned, vec!["X".to_string()]);
        assert!(g.preferred.is_empty());
        assert!(g.proper_nouns.is_empty());
    }

    /// 35.2 — Punctuation with single suggestion is auto-fix safe.
    #[test]
    fn explain_meta_punctuation_is_auto_fix_safe() {
        let issue = Issue::new(
            0,
            1,
            ",",
            vec!["，".into()],
            IssueType::Punctuation,
            Severity::Warning,
        );
        let meta = derive_explain_meta(&issue);
        assert!(meta.auto_fix_safe);
        assert!(!meta.needs_review);
    }

    #[test]
    fn issue_summary_omits_zero_sampling_fields() {
        let summary = IssueSummary {
            errors: 1,
            warnings: 2,
            info: 0,
            tm_suppressed: 0,
            tier2_resolved: 0,
            tier2_gray_zone: 0,
            sampling_used: 0,
            sampling_skipped: 0,
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(!json.contains("sampling_used"));
        assert!(!json.contains("sampling_skipped"));
        assert!(!json.contains("tm_suppressed"));
    }

    #[test]
    fn issue_summary_includes_nonzero_sampling_fields() {
        let summary = IssueSummary {
            errors: 0,
            warnings: 3,
            info: 1,
            tm_suppressed: 0,
            tier2_resolved: 0,
            tier2_gray_zone: 0,
            sampling_used: 2,
            sampling_skipped: 5,
        };
        let json = serde_json::to_string(&summary).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["sampling_used"], 2);
        assert_eq!(parsed["sampling_skipped"], 5);
        // tm_suppressed still omitted when zero.
        assert!(parsed.get("tm_suppressed").is_none());
    }

    #[test]
    fn build_summary_threads_sampling_stats() {
        let issues = vec![
            Issue::new(0, 3, "foo", vec![], IssueType::CrossStrait, Severity::Error),
            Issue::new(
                3,
                3,
                "bar",
                vec![],
                IssueType::CrossStrait,
                Severity::Warning,
            ),
        ];
        let stats = SamplingStats {
            used: 3,
            skipped: 7,
        };
        let disambig = DisambigStats::default();
        let summary = build_summary(&issues, 1, stats, &disambig);
        assert_eq!(summary.errors, 1);
        assert_eq!(summary.warnings, 1);
        assert_eq!(summary.tm_suppressed, 1);
        assert_eq!(summary.sampling_used, 3);
        assert_eq!(summary.sampling_skipped, 7);
    }

    #[test]
    fn build_explanation_for_spaced_acronym_is_not_duplicate_text() {
        let issue = Issue::new(
            0,
            5,
            "C P U",
            vec!["CPU".into()],
            IssueType::Repetition,
            Severity::Info,
        );
        let explanation = build_explanation(&issue).expect("explanation");
        assert!(explanation.contains("CPU"));
        assert!(explanation.contains("transcription artifact"));
        assert!(!explanation.contains("consecutive duplicate"));
    }

    #[test]
    fn build_explanation_for_translationese_does_not_duplicate_context() {
        // Regression: the main Translationese arm already appends the
        // context, so the shared "Context:" tail must be suppressed for
        // this issue type or the narrative gets repeated.
        let issue = Issue::new(
            0,
            3,
            "透過",
            vec!["藉由".into(), "經由".into()],
            IssueType::Translationese,
            Severity::Info,
        )
        .with_context("dewesternise.V3: abstract-means calque; prefer 藉由");
        let explanation = build_explanation(&issue).expect("explanation");
        assert_eq!(
            explanation.matches("dewesternise.V3").count(),
            1,
            "context must appear exactly once: {explanation}"
        );
        assert!(explanation.contains("Suggested rewrite"));
    }

    #[test]
    fn build_explanation_for_repetition_keeps_duplicate_text() {
        let issue = Issue::new(
            0,
            6,
            "cache cache",
            vec!["cache".into()],
            IssueType::Repetition,
            Severity::Info,
        );
        let explanation = build_explanation(&issue).expect("explanation");
        assert!(explanation.contains("consecutive duplicate"));
    }

    #[test]
    fn resolution_tier_classify_deterministic() {
        let issue = Issue::new(0, 3, "foo", vec![], IssueType::Punctuation, Severity::Error);
        assert_eq!(
            ResolutionTier::classify(&issue),
            ResolutionTier::Deterministic
        );
    }

    #[test]
    fn resolution_tier_classify_heuristic() {
        let mut issue = Issue::new(
            0,
            3,
            "foo",
            vec!["bar".into()],
            IssueType::CrossStrait,
            Severity::Warning,
        );
        issue.tier2_outcome = Tier2Outcome::Resolved;
        assert_eq!(ResolutionTier::classify(&issue), ResolutionTier::Heuristic);
    }

    #[test]
    fn resolution_tier_classify_llm_judged() {
        let mut issue = Issue::new(
            0,
            3,
            "foo",
            vec!["bar".into()],
            IssueType::CrossStrait,
            Severity::Warning,
        );
        issue.tier2_outcome = Tier2Outcome::GrayZone;
        issue.llm_judged = true;
        assert_eq!(ResolutionTier::classify(&issue), ResolutionTier::LlmJudged);
    }

    #[test]
    fn resolution_tier_classify_unresolved_gray_zone() {
        let mut issue = Issue::new(
            0,
            3,
            "foo",
            vec!["bar".into()],
            IssueType::CrossStrait,
            Severity::Warning,
        );
        issue.tier2_outcome = Tier2Outcome::GrayZone;
        // No LLM annotation — stays unresolved.
        assert_eq!(ResolutionTier::classify(&issue), ResolutionTier::Unresolved);
    }

    #[test]
    fn resolution_tier_classify_suppressed() {
        let mut issue = Issue::new(0, 3, "foo", vec![], IssueType::CrossStrait, Severity::Info);
        issue.tier2_outcome = Tier2Outcome::Suppressed;
        assert_eq!(ResolutionTier::classify(&issue), ResolutionTier::Unresolved);
    }

    #[test]
    fn summary_metrics_counts_tiers() {
        let mut issues = vec![
            Issue::new(0, 1, "a", vec![], IssueType::Punctuation, Severity::Error),
            Issue::new(
                1,
                1,
                "b",
                vec!["c".into()],
                IssueType::CrossStrait,
                Severity::Warning,
            ),
            Issue::new(
                2,
                1,
                "d",
                vec!["e".into()],
                IssueType::CrossStrait,
                Severity::Warning,
            ),
            Issue::new(
                3,
                1,
                "f",
                vec!["g".into()],
                IssueType::Confusable,
                Severity::Warning,
            ),
        ];
        issues[1].tier2_outcome = Tier2Outcome::Resolved;
        issues[2].tier2_outcome = Tier2Outcome::GrayZone;
        issues[2].llm_judged = true;
        issues[3].tier2_outcome = Tier2Outcome::Suppressed;

        let stats = SamplingStats {
            used: 1,
            skipped: 0,
        };
        let metrics = build_summary_metrics(&issues, &stats, None);

        assert_eq!(metrics.deterministic_fixes, 1);
        assert_eq!(metrics.heuristic_fixes, 1);
        assert_eq!(metrics.llm_judged_fixes, 1);
        assert_eq!(metrics.unresolved, 1);
        assert_eq!(metrics.llm_calls, 1);
        assert_eq!(metrics.llm_tokens, 0);
        assert_eq!(metrics.confidence_distribution.high, 2);
        assert_eq!(metrics.confidence_distribution.medium, 1);
        assert_eq!(metrics.confidence_distribution.low, 1);
    }

    #[test]
    fn summary_metrics_omitted_without_flag() {
        let issues = vec![Issue::new(
            0,
            3,
            "foo",
            vec![],
            IssueType::CrossStrait,
            Severity::Error,
        )];
        let disambig = DisambigStats::default();
        let summary = build_summary(&issues, 0, SamplingStats::default(), &disambig);
        let output = SummaryOutput {
            accepted: true,
            summary: &summary,
            gate: GateInfo {
                enabled: false,
                max_errors: 0,
                residual_errors: 1,
                max_warnings: 0,
                residual_warnings: 0,
            },
            profile: "base",
            detected_script: "traditional",
            coverage: None,
            oral_density: None,
            quality_flags: None,
            ai_signature: None,
            translationese_signature: None,
            style_scorecard: None,
            telemetry: None,
            summary_metrics: None,
        };
        let json = serde_json::to_string(&output).unwrap();
        assert!(!json.contains("summary_metrics"));
        assert!(!json.contains("deterministic_fixes"));
    }

    #[test]
    fn resolution_tier_serializes_snake_case() {
        let tier = ResolutionTier::LlmJudged;
        let json = serde_json::to_value(tier).unwrap();
        assert_eq!(json, serde_json::json!("llm_judged"));
    }

    #[test]
    fn build_summary_excludes_hard_anchors_from_tier2_resolved() {
        let issues = vec![Issue::new(
            0,
            3,
            "foo",
            vec!["bar".into()],
            IssueType::CrossStrait,
            Severity::Warning,
        )];
        let disambig = DisambigStats {
            hard_anchor: 2,
            tier2_resolved: 3,
            suppressed: 0,
            gray_zone: 1,
            not_eligible: 0,
        };

        let summary = build_summary(&issues, 0, SamplingStats::default(), &disambig);
        assert_eq!(summary.tier2_resolved, 3);
        assert_eq!(summary.tier2_gray_zone, 1);
    }

    fn make_initialized_server() -> (Server, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut server = Server::new(
            OverrideStore::open(&dir.path().join("overrides.json")).unwrap(),
            SuppressionStore::open(&dir.path().join("suppressions.json")).unwrap(),
            PackStore::new(dir.path().join("packs")),
            vec![],
            None,
        )
        .unwrap();
        let mut init_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(RequestId::Int(0)),
            method: "initialize".into(),
            params: serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.1" }
            }),
        };
        let resp = server.dispatch_preinit(&mut init_req);
        assert!(resp.unwrap().unwrap().error.is_none());

        (server, dir)
    }

    fn call_zhtw(server: &mut Server, args: serde_json::Value) -> JsonRpcResponse {
        let mut req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(RequestId::Int(1)),
            method: "tools/call".into(),
            params: serde_json::json!({ "name": "zhtw", "arguments": args }),
        };
        server.handle_tools_call(&mut req, None)
    }

    fn assert_tool_success(resp: &JsonRpcResponse) -> serde_json::Value {
        let result = resp.result.as_ref().unwrap();
        assert!(result.get("isError").is_none());
        let content = result.get("content").and_then(|v| v.as_array()).unwrap();
        assert!(!content.is_empty());
        let text = content[0].get("text").and_then(|v| v.as_str()).unwrap();
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn tools_call_simplified_input_builds_s2t_lazily() {
        // s2t is built lazily on first Simplified input; this exercises that
        // path end-to-end (get_or_init + convert) and confirms the flag is set.
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(&mut server, serde_json::json!({ "text": "软件测试" }));
        let output = assert_tool_success(&resp);
        assert_eq!(output["s2t_applied"], true);
        // A second call reuses the already-built converter (no re-init panic).
        let resp2 = call_zhtw(&mut server, serde_json::json!({ "text": "软件" }));
        assert_eq!(assert_tool_success(&resp2)["s2t_applied"], true);
    }

    #[test]
    fn tools_call_arguments_not_object() {
        let (mut server, _dir) = make_initialized_server();
        let mut req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(RequestId::Int(1)),
            method: "tools/call".into(),
            params: serde_json::json!({ "name": "zhtw", "arguments": "not_an_object" }),
        };
        let resp = server.handle_tools_call(&mut req, None);
        assert!(resp.error.is_some());
        let err = resp.error.unwrap();
        assert_eq!(err.code, INVALID_PARAMS);
    }

    #[test]
    fn tools_call_text_exceeds_max_size() {
        let (mut server, _dir) = make_initialized_server();
        let big_text = "あ".repeat(Server::MAX_TEXT_BYTES + 1);
        let resp = call_zhtw(&mut server, serde_json::json!({ "text": big_text }));
        assert!(resp.error.is_some());
        let err = resp.error.unwrap();
        assert_eq!(err.code, INVALID_PARAMS);
    }

    #[test]
    fn tools_call_detect_style_rejected_for_tabular_output() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "正確的軟體",
                "output": "tabular",
                "detect_style": true
            }),
        );
        assert!(resp.error.is_some());
        let err = resp.error.unwrap();
        assert_eq!(err.code, INVALID_PARAMS);
        assert!(err.message.contains("detect_style"));
    }

    #[test]
    fn tools_call_empty_text_input() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(&mut server, serde_json::json!({ "text": "" }));
        assert!(resp.error.is_none());
        let output = assert_tool_success(&resp);
        assert_eq!(output["accepted"], true);
        assert_eq!(output["gate"]["enabled"], false);
        assert_eq!(output["text"], "");
    }

    #[test]
    fn known_params_list_includes_all_documented_params() {
        // Round-4 validation regression: every parameter that the schema
        // documents must also be in zhtw_known_params(), or the strict
        // validator rejects valid clients with -32602.
        let known: std::collections::HashSet<&str> = zhtw_known_params().iter().copied().collect();
        for p in [
            "text",
            "fix_mode",
            "max_errors",
            "max_warnings",
            "profile",
            "relaxed",
            "exempt_blockquotes",
            "content_type",
            "political_stance",
            "ignore_terms",
            "glossary",
            "consistency",
            "explain",
            "fix_output",
            "output",
            "detect_ai",
            "detect_translationese",
            "detect_style",
            "translationese_domain",
            "ai_threshold",
            "include_telemetry",
            "include_stats",
        ] {
            assert!(
                known.contains(p),
                "documented parameter {p:?} missing from zhtw_known_params()",
            );
        }
    }

    #[test]
    fn tools_call_response_gate_accepts_no_errors_when_max_errors_set() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({ "text": "", "max_errors": 0 }),
        );
        assert!(resp.error.is_none());
        let output = assert_tool_success(&resp);
        assert_eq!(output["accepted"], true);
        assert_eq!(output["gate"]["enabled"], true);
        assert_eq!(output["gate"]["max_errors"], 0);
    }

    fn assert_tool_rejected(resp: &JsonRpcResponse) -> serde_json::Value {
        let result = resp.result.as_ref().unwrap();
        assert_eq!(result.get("isError").and_then(|v| v.as_bool()), Some(true));
        let content = result.get("content").and_then(|v| v.as_array()).unwrap();
        assert!(!content.is_empty());
        let text = content[0].get("text").and_then(|v| v.as_str()).unwrap();
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn tools_call_response_gate_rejects_when_errors_exceed_limit() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({ "text": "乞業", "max_errors": 0 }),
        );
        let output = assert_tool_rejected(&resp);
        assert_eq!(output["accepted"], false);
        assert_eq!(output["gate"]["enabled"], true);
        assert!(output["gate"]["residual_errors"].as_u64().unwrap() > 0);
    }

    #[test]
    fn tools_call_response_gate_accepts_when_errors_within_limit() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({ "text": "乞業", "max_errors": 10 }),
        );
        let output = assert_tool_success(&resp);
        assert_eq!(output["accepted"], true);
        assert_eq!(output["gate"]["enabled"], true);
    }

    #[test]
    fn tools_call_response_gate_enabled_when_only_max_warnings_set() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({ "text": "", "max_warnings": 0 }),
        );
        assert!(resp.error.is_none());
        let output = assert_tool_success(&resp);
        assert_eq!(output["accepted"], true);
        assert_eq!(output["gate"]["enabled"], true);
        assert_eq!(output["gate"]["max_warnings"], 0);
    }

    #[test]
    fn tools_call_response_gate_rejects_when_warnings_exceed_limit() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({ "text": "軟件", "max_warnings": 0 }),
        );
        let output = assert_tool_rejected(&resp);
        assert_eq!(output["accepted"], false);
        assert_eq!(output["gate"]["enabled"], true);
        assert!(output["gate"]["residual_warnings"].as_u64().unwrap() > 0);
    }

    #[test]
    fn tools_call_response_gate_accepts_when_warnings_within_limit() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({ "text": "軟件", "max_warnings": 10 }),
        );
        let output = assert_tool_success(&resp);
        assert_eq!(output["accepted"], true);
        assert_eq!(output["gate"]["enabled"], true);
    }

    #[test]
    fn tools_call_response_gate_rejects_when_errors_pass_but_warnings_exceed() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "軟件", "max_errors": 10, "max_warnings": 0
            }),
        );
        let output = assert_tool_rejected(&resp);
        assert_eq!(output["accepted"], false);
        assert_eq!(output["gate"]["enabled"], true);
    }

    #[test]
    fn tools_call_response_gate_rejects_when_warnings_pass_but_errors_exceed() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "乞業", "max_errors": 0, "max_warnings": 10
            }),
        );
        let output = assert_tool_rejected(&resp);
        assert_eq!(output["accepted"], false);
        assert_eq!(output["gate"]["enabled"], true);
    }

    #[test]
    fn tools_call_response_gate_accepts_after_stance_filters_political_errors() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "內地", "fix_mode": "none", "max_errors": 0, "political_stance": "neutral"
            }),
        );
        let output = assert_tool_success(&resp);
        assert_eq!(output["accepted"], true);
    }

    #[test]
    fn tools_call_response_gate_rejects_when_stance_keeps_political_errors() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "內地", "fix_mode": "none", "max_errors": 0, "political_stance": "roc_centric"
            }),
        );
        let output = assert_tool_rejected(&resp);
        assert_eq!(output["accepted"], false);
    }

    #[test]
    fn tools_call_response_gate_accepts_after_ignore_terms_downgrades_error() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "乞業", "max_errors": 0, "ignore_terms": ["乞業"]
            }),
        );
        let output = assert_tool_success(&resp);
        assert_eq!(output["accepted"], true);
    }

    #[test]
    fn tools_call_fix_respects_exempt_blockquotes() {
        let (mut server, _dir) = make_initialized_server();
        let text = "> 用戶輸入需要驗證。\n";
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": text,
                "content_type": "markdown",
                "exempt_blockquotes": true,
                "fix_mode": "lexical_safe"
            }),
        );
        let output = assert_tool_success(&resp);
        assert_eq!(output["text"], text);
        let issues = output["issues"].as_array().expect("issues array");
        assert!(
            !issues.iter().any(|i| i["found"] == "用戶"),
            "blockquote text must stay exempt on MCP fix path; got {issues:?}"
        );
    }

    #[test]
    fn tools_call_fix_honors_glossary_banned_terms() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "ABC 不該出現在文件中。\n",
                "fix_mode": "lexical_safe",
                "glossary": { "banned": ["ABC"] }
            }),
        );
        let output = assert_tool_success(&resp);
        let issues = output["issues"].as_array().expect("issues array");
        assert!(
            issues.iter().any(|i| i["found"] == "ABC"),
            "glossary banned terms must remain active on fix path; got {issues:?}"
        );
    }

    #[test]
    fn tools_call_fix_honors_glossary_proper_nouns() {
        let (mut server, _dir) = make_initialized_server();
        let text = "我們的線程實作。\n";
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": text,
                "fix_mode": "lexical_safe",
                "glossary": { "proper_nouns": ["線程"] }
            }),
        );
        let output = assert_tool_success(&resp);
        assert_eq!(output["text"], text);
        let issues = output["issues"].as_array().expect("issues array");
        assert!(
            !issues.iter().any(|i| i["found"] == "線程"),
            "proper_nouns must suppress fix-path issues; got {issues:?}"
        );
    }

    #[test]
    fn tools_call_fix_returns_consistency_report() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "我們的線程太慢，需要重構執行緒。\n",
                "fix_mode": "orthographic",
                "consistency": true
            }),
        );
        let output = assert_tool_success(&resp);
        let groups = output["consistency"]["groups"]
            .as_array()
            .expect("consistency groups");
        assert!(
            groups.iter().any(|g| g["term_group"] == "thread"),
            "fix-path consistency report must be returned; got {groups:?}"
        );
    }

    #[test]
    fn tools_call_set_invalid_content_type() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({ "text": "", "content_type": "invalid_type" }),
        );
        assert!(resp.error.is_some());
        assert!(resp.result.is_none());
        let err = resp.error.unwrap();
        assert_eq!(err.code, INVALID_PARAMS);
        let data = err.data.unwrap();
        assert_eq!(data["field"], "content_type");
        assert_eq!(data["value"], "invalid_type");
    }

    #[test]
    fn tools_call_set_invalid_profile() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({ "text": "", "profile": "invalid_profile" }),
        );
        assert!(resp.error.is_some());
        assert!(resp.result.is_none());
        let err = resp.error.unwrap();
        assert_eq!(err.code, INVALID_PARAMS);
        let data = err.data.unwrap();
        assert_eq!(data["field"], "profile");
        assert_eq!(data["value"], "invalid_profile");
    }

    #[test]
    fn tools_call_set_invalid_fix_mode() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({ "text": "", "fix_mode": "invalid_fix_mode" }),
        );
        assert!(resp.error.is_some());
        assert!(resp.result.is_none());
        let err = resp.error.unwrap();
        assert_eq!(err.code, INVALID_PARAMS);
        let data = err.data.unwrap();
        assert_eq!(data["field"], "fix_mode");
        assert_eq!(data["value"], "invalid_fix_mode");
    }

    #[test]
    fn tools_call_set_invalid_political_stance() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({ "text": "", "political_stance": "invalid_stance" }),
        );
        assert!(resp.error.is_some());
        assert!(resp.result.is_none());
        let err = resp.error.unwrap();
        assert_eq!(err.code, INVALID_PARAMS);
        let data = err.data.unwrap();
        assert_eq!(data["field"], "political_stance");
        assert_eq!(data["value"], "invalid_stance");
    }

    #[test]
    fn tools_call_explain_true_includes_explanation() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "軟件", "explain": true, "output": "full"
            }),
        );
        let output = assert_tool_success(&resp);
        let issues = output["issues"].as_array().unwrap();
        assert!(!issues.is_empty());
        assert!(issues[0].get("explanation").is_some());
    }

    #[test]
    fn tools_call_explain_false_omits_explanation() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "軟件", "explain": false, "output": "full"
            }),
        );
        let output = assert_tool_success(&resp);
        let issues = output["issues"].as_array().unwrap();
        assert!(!issues.is_empty());
        assert!(issues[0].get("explanation").is_none());
    }

    #[test]
    fn tools_call_explain_non_bool_treated_as_false() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "軟件", "explain": "not_a_boolean", "output": "full"
            }),
        );
        let output = assert_tool_success(&resp);
        let issues = output["issues"].as_array().unwrap();
        assert!(!issues.is_empty());
        assert!(issues[0].get("explanation").is_none());
    }

    #[test]
    fn tools_call_explain_true_compact_includes_explanation() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "軟件", "explain": true, "output": "compact"
            }),
        );
        let output = assert_tool_success(&resp);
        let issues = output["issues"].as_array().unwrap();
        assert!(!issues.is_empty());
        assert!(issues[0].get("explanation").is_some());
        assert!(issues[0].get("count").is_some());
        assert!(issues[0].get("locations").is_some());
    }

    #[test]
    fn tools_call_explain_false_compact_omits_explanation() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "軟件", "explain": false, "output": "compact"
            }),
        );
        let output = assert_tool_success(&resp);
        let issues = output["issues"].as_array().unwrap();
        assert!(!issues.is_empty());
        assert!(issues[0].get("explanation").is_none());
        assert!(issues[0].get("count").is_some());
        assert!(issues[0].get("locations").is_some());
    }

    #[test]
    fn tools_call_lint_stance_roc_centric_keeps_political_issue() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "內地", "fix_mode": "none", "political_stance": "roc_centric"
            }),
        );
        let output = assert_tool_success(&resp);
        let issues = output["issues"].as_array().unwrap();
        assert!(issues
            .iter()
            .any(|i| i["rule_type"] == "political_coloring"));
    }

    #[test]
    fn tools_call_lint_stance_neutral_removes_political_issue() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "內地", "fix_mode": "none", "political_stance": "neutral"
            }),
        );
        let output = assert_tool_success(&resp);
        let issues = output["issues"].as_array().unwrap();
        assert!(!issues
            .iter()
            .any(|i| i["rule_type"] == "political_coloring"));
    }

    #[test]
    fn tools_call_full_output_includes_scan_metadata() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "使用 C P U 架構處理工作負載",
                "output": "full"
            }),
        );
        let output = assert_tool_success(&resp);
        assert!(output["coverage"]["rules_checked"].as_u64().unwrap() > 0);
        assert_eq!(output["coverage"]["rules_matched"], 0);
        assert!(output["quality_flags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "spaced_acronyms"));
    }

    #[test]
    fn tools_call_summary_output_keeps_document_level_flags() {
        let (mut server, _dir) = make_initialized_server();
        let resp = call_zhtw(
            &mut server,
            serde_json::json!({
                "text": "這個那個這個那個這個那個這個那個這個那個",
                "output": "summary"
            }),
        );
        let output = assert_tool_success(&resp);
        assert_eq!(output["summary"]["errors"], 0);
        assert_eq!(output["summary"]["warnings"], 0);
        assert_eq!(output["summary"]["info"], 0);
        assert_eq!(output["oral_density"], 1.0);
        assert!(output["quality_flags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "high_oral_density"));
    }
}
