# MoE Standards Summary

Reference for implementors. Not actionable on its own. Tables are illustrative subsets,
not exhaustive inventories. Cross-reference against `assets/ruleset.json` for the canonical
rule list.

## Punctuation (重訂標點符號手冊)
| Mark | Half-width | Full-width (MoE) | Notes |
|------|-----------|-------------------|-------|
| Comma | `,` U+002C | `，` U+FF0C | Always full-width in CJK prose |
| Period | `.` U+002E | `。` U+3002 | Hollow circle, not solid dot |
| Colon | `:` U+003A | `：` U+FF1A | Exception: UI string contexts |
| Semicolon | `;` U+003B | `；` U+FF1B | |
| Exclamation | `!` U+0021 | `！` U+FF01 | |
| Question | `?` U+003F | `？` U+FF1F | |
| Enum comma | (none) | `、` U+3001 | For coordinate lists only |
| Primary quote | `"` `"` | `「` `」` | U+300C / U+300D |
| Secondary quote | | `『` `』` | U+300E / U+300F (nested only) |
| Book title | | `《` `》` | U+300A / U+300B |
| Ellipsis | `...` | `……` | Two U+2026 characters (6 dots total) |
| Em dash | `--` | `──` | Two U+2500 or U+2014 characters |

## Cross-strait term divergence (high-density domains)
| zh-CN | zh-TW | English | Notes |
|-------|-------|---------|-------|
| 信息 | 資訊 | Information | 信息 in TW often means "message"; domain-dependent |
| 軟件 | 軟體 | Software | |
| 硬件 | 硬體 | Hardware | |
| 網絡 | 網路 | Network | |
| 默認 | 預設 | Default | 默認 in TW means "tacit approval" |
| 打印 | 列印 | Print | |
| 質量 | 品質 | Quality | 質量 in TW physics = "mass" |
| 視頻 | 影片 | Video | |
| 屏幕 | 螢幕 | Screen | |
| 程序 | 程式 | Program | 程序 in TW = "procedure"; domain-dependent |
| 鼠標 | 滑鼠 | Mouse | |
| 接口 | 介面 | Interface | 接口 in TW for physical port = 連接埠; domain-dependent |

## Character variants (國字標準字體)
| Non-standard | MoE standard | Notes |
|-------------|-------------|-------|
| 裏 | 裡 | "inside" -- Kangxi vs MoE |
| 綫 | 線 | "thread/line" |
| 麪 | 麵 | "noodle" |
| 着 | 著 | particle usage unified under 著 in TW; exception for chess (下著) and proper nouns |
| 台 | 臺 | Lexical contexts only: 臺灣/臺北/臺中/臺南; 平台/月台/舞台/台詞 keep 台 |
