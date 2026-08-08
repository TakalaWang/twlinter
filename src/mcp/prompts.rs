// MCP Prompts: expose editorial persona prompts.
//
// Three prompts:
//   normalize_tone     — ground an LLM in MoE-standard zh-TW writing conventions
//   lint_natural       — translate free-form requests into zhtw calls
//   editorial_review   — multi-turn editorial workflow persona

use super::types::{PromptArgDef, PromptContent, PromptDef, PromptGetResult, PromptMessage};

/// The normalize_tone prompt name.
pub const NORMALIZE_TONE: &str = "normalize_tone";

/// The lint_natural prompt name.
pub const LINT_NATURAL: &str = "lint_natural";

/// The editorial_review prompt name.
pub const EDITORIAL_REVIEW: &str = "editorial_review";

/// Return the list of available prompts.
pub fn list_prompts() -> Vec<PromptDef> {
    vec![
        PromptDef {
            name: NORMALIZE_TONE.into(),
            description: "Ground an LLM in MoE-standard zh-TW conventions".into(),
            arguments: None,
        },
        PromptDef {
            name: LINT_NATURAL.into(),
            description: "Translate a free-form instruction into a zhtw call".into(),
            arguments: Some(vec![
                PromptArgDef {
                    name: "instruction".into(),
                    description: "Free-form instruction, e.g. 'check for mainland terms'".into(),
                    required: true,
                },
                PromptArgDef {
                    name: "text".into(),
                    description: "The text to lint".into(),
                    required: true,
                },
            ]),
        },
        PromptDef {
            name: EDITORIAL_REVIEW.into(),
            description:
                "Multi-turn zh-TW editorial workflow: review, fix, re-check until accepted".into(),
            arguments: Some(vec![
                PromptArgDef {
                    name: "text".into(),
                    description: "Draft text to review and refine".into(),
                    required: true,
                },
                PromptArgDef {
                    name: "max_iterations".into(),
                    description: "Maximum review-fix cycles (default: 3)".into(),
                    required: false,
                },
            ]),
        },
    ]
}

/// Get a prompt by name, substituting arguments.
pub fn get_prompt(
    name: &str,
    arguments: &std::collections::HashMap<String, String>,
) -> Option<PromptGetResult> {
    match name {
        NORMALIZE_TONE => Some(get_normalize_tone()),
        LINT_NATURAL => Some(get_lint_natural(arguments)),
        EDITORIAL_REVIEW => Some(get_editorial_review(arguments)),
        _ => None,
    }
}

fn get_normalize_tone() -> PromptGetResult {
    let system_text = r#"You are writing in Traditional Chinese (Taiwan) following Ministry of Education (教育部) standards. Adhere to these conventions:

## Punctuation
- Use full-width punctuation in CJK prose: ，。：；！？（）
- Use 「」 for primary quotation marks, 『』 for nested quotes (not "" or '')
- Use 、 (dunhao) for enumerating items in a list, not ，
- Use 《》 for book/publication titles
- Use ～ for ranges in prose (e.g., 第一～第五)

## Vocabulary
- Use Taiwan-standard terms, not Mainland China equivalents:
  - 軟體 (not 軟件), 硬體 (not 硬件), 網路 (not 網絡)
  - 資訊 (not 信息), 預設 (not 默認), 列印 (not 打印)
  - 品質 (not 質量 for "quality"), 影片 (not 視頻)
  - 螢幕 (not 屏幕), 程式 (not 程序 for "program")
  - 滑鼠 (not 鼠標), 介面 (not 接口 for "interface")
  - 伺服器 (not 服務器), 記憶體 (not 內存)
  - 資料庫 (not 數據庫), 演算法 (not 算法)

## Character Forms
- Use MoE standard character forms (國字標準字體):
  - 裡 (not 裏), 線 (not 綫), 麵 (not 麪), 著 (not 着 as particle)

## Register
- Professional, clear prose suitable for technical and business writing in Taiwan
- Avoid overly formal archaic expressions and Mainland phrasing patterns (e.g., 進行 as a dummy verb, 的話 as conditional)
- When uncertain about a term, prefer the form commonly used in Taiwan's tech industry and media

Reference: zh-tw://style-guide/moe for the complete style guide."#;

    PromptGetResult {
        description: "Grounds the LLM in MoE-standard zh-TW vocabulary, punctuation, and character forms for professional technical writing".into(),
        messages: vec![PromptMessage {
            role: "user".into(),
            content: PromptContent {
                content_type: "text".into(),
                text: system_text.into(),
            },
        }],
    }
}

/// Extract a prompt argument as a &str, defaulting to "".
fn arg_str<'a>(args: &'a std::collections::HashMap<String, String>, key: &str) -> &'a str {
    args.get(key).map(|s| s.as_str()).unwrap_or("")
}

/// Build the lint_natural prompt: instructs the host LLM to parse a free-form
/// instruction and the provided text into a structured zhtw tool call.
fn get_lint_natural(args: &std::collections::HashMap<String, String>) -> PromptGetResult {
    let instruction = arg_str(args, "instruction");
    let text = arg_str(args, "text");

    let system_text = format!(
        r#"You are a zh-TW text linting assistant. Parse the user's instruction and text, then call the `zhtw` MCP tool with the appropriate parameters.

## Parameter Extraction Rules

From the instruction, extract:
- `fix_mode`: "lexical_safe" if the user asks to fix/correct/repair; "lexical_contextual" if they want all fixes; "orthographic" for punctuation/spacing only; "none" otherwise
- `profile`: "strict" if they mention MoE/standard forms/variants; "base" otherwise
- `relaxed`: true if the context is software UI strings (half-width colons allowed, no grammar); false otherwise
- `detect_ai`: true if they mention AI writing/filler/naturalness review (can combine with any profile); false otherwise
- `ai_threshold`: "low" if they want sensitive/strict AI detection; "high" if they want conservative/lenient AI detection; "medium" (default) otherwise. Only meaningful when `detect_ai` is true.
- `max_errors`: integer if the user specifies a rejection threshold (e.g. "reject if more than 3 errors", "no errors allowed" = 0). Omit if no gate requested.
- `political_stance`: "neutral" if they ask for neutral/apolitical; "international" for international style; "roc_centric" (default)
- `ignore_terms`: any terms the user explicitly says to skip/ignore
- `content_type`: "markdown" if the text contains Markdown; "plain" otherwise
- `explain`: true if the user asks for explanations/reasons; false otherwise
- `output`: "compact" unless the user needs byte offsets or full detail

## Instruction
{instruction}

## Text to Check
```
{text}
```

Now call `zhtw` with the extracted parameters. Return only the tool call, no commentary."#
    );

    PromptGetResult {
        description: "Translates a free-form lint instruction into a zhtw tool call".into(),
        messages: vec![PromptMessage {
            role: "user".into(),
            content: PromptContent {
                content_type: "text".into(),
                text: system_text,
            },
        }],
    }
}

/// Build the editorial_review prompt: multi-turn persona for iterative
/// zh-TW text review and refinement.
fn get_editorial_review(args: &std::collections::HashMap<String, String>) -> PromptGetResult {
    let text = arg_str(args, "text");
    let max_iterations: u32 = args
        .get("max_iterations")
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);

    let system_text = format!(
        r#"You are an experienced Traditional Chinese (Taiwan) editor following Ministry of Education (教育部) standards. Your task is to iteratively review and refine the provided text.

## Workflow

For each iteration (up to {max_iterations} total):
1. Call `zhtw` with the current text: `{{ "text": "...", "detect_ai": true, "fix_mode": "lexical_safe", "explain": true, "output": "compact", "content_type": "markdown" }}`
2. If `accepted: true` with 0 errors, the text is finalized. Present the clean text.
3. If issues remain:
   a. Explain each issue in context — why MoE prefers the standard form, cultural background
   b. Apply safe fixes and present the updated text
   c. For ambiguous terms (multiple suggestions), explain the options and pick the best fit
   d. Re-check with the updated text (next iteration)

## Constraints
- Maximum {max_iterations} review cycles. If issues persist after {max_iterations} iterations, present the best version with a note about remaining items.
- Never silently drop issues. Every change must be explained.
- Preserve the author's voice and intent. Fix terminology and punctuation, not style.
- Use `explain: true` to provide culturally relevant explanations.

## Draft Text
```
{text}
```

Begin your first review iteration now."#
    );

    PromptGetResult {
        description:
            "Multi-turn zh-TW editorial review: iteratively fix and explain until accepted".into(),
        messages: vec![PromptMessage {
            role: "user".into(),
            content: PromptContent {
                content_type: "text".into(),
                text: system_text,
            },
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_args() -> std::collections::HashMap<String, String> {
        std::collections::HashMap::new()
    }

    #[test]
    fn list_returns_three_prompts() {
        let prompts = list_prompts();
        assert_eq!(prompts.len(), 3);
        assert_eq!(prompts[0].name, NORMALIZE_TONE);
        assert_eq!(prompts[1].name, LINT_NATURAL);
        assert_eq!(prompts[2].name, EDITORIAL_REVIEW);
    }

    #[test]
    fn get_normalize_tone_returns_content() {
        let result = get_prompt(NORMALIZE_TONE, &empty_args()).unwrap();
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0].role, "user");
        assert!(result.messages[0]
            .content
            .text
            .contains("Traditional Chinese"));
        assert!(result.messages[0].content.text.contains("軟體"));
    }

    #[test]
    fn get_lint_natural_includes_instruction_and_text() {
        let mut args = std::collections::HashMap::new();
        args.insert("instruction".into(), "check for mainland terms".into());
        args.insert("text".into(), "這個軟件很好用".into());
        let result = get_prompt(LINT_NATURAL, &args).unwrap();
        assert_eq!(result.messages.len(), 1);
        assert!(result.messages[0]
            .content
            .text
            .contains("check for mainland terms"));
        assert!(result.messages[0].content.text.contains("這個軟件很好用"));
        assert!(result.messages[0].content.text.contains("zhtw"));
    }

    #[test]
    fn get_editorial_review_includes_text_and_max_iterations() {
        let mut args = std::collections::HashMap::new();
        args.insert("text".into(), "使用默認設置".into());
        args.insert("max_iterations".into(), "5".into());
        let result = get_prompt(EDITORIAL_REVIEW, &args).unwrap();
        assert_eq!(result.messages.len(), 1);
        assert!(result.messages[0].content.text.contains("使用默認設置"));
        assert!(result.messages[0].content.text.contains("up to 5 total"));
    }

    #[test]
    fn get_editorial_review_default_iterations() {
        let mut args = std::collections::HashMap::new();
        args.insert("text".into(), "測試文字".into());
        let result = get_prompt(EDITORIAL_REVIEW, &args).unwrap();
        assert!(result.messages[0].content.text.contains("up to 3 total"));
    }

    #[test]
    fn get_unknown_returns_none() {
        assert!(get_prompt("unknown", &empty_args()).is_none());
    }
}
