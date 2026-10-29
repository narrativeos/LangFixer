//! 多模态审校（Multimodal Proofreading）核心层
//!
//! 设计全文见仓库根目录 `MULTIMODAL.md`。一句话职责划分：
//!
//! ```text
//! 载体（图片/PDF/Word） ──adapter──▶ TextUnit[] ──stitch──▶ stitched text
//!                                                        │
//!                                          既有检测内核（不改）detect(&str)→Error[]
//!                                                        │
//! 回写原语 ◀──adapter── Annotation[] ◀──remap── offset map（span ↔ unit）
//! ```
//!
//! 本 crate **只做**：模型（[`model`]）、拼接与回映射（[`stitch`]，纯函数）、
//! 协作工具契约（[`adapter`]）、各载体适配器（[`photo`]）、视觉输出解析（[`vision`]，
//! 网络调用在 api-server）、gold 集评测（[`gold`]）、正误表导出（[`report`]）。
//!
//! 铁律：检测内核不知道「图片」是什么——`Error.span` 始终是 stitched text 的字节偏移，
//! 载体差异全部收敛到 `Region` + `Stitched.unit_ranges`。

pub mod adapter;
pub mod casebase;
pub mod docparse;
pub mod gold;
pub mod model;
pub mod pdfcraft;
pub mod photo;
pub mod report;
pub mod stitch;
pub mod vision;
pub mod wordcraft;

pub use adapter::{ApplyRequest, ApplyResult, Collaborator, MediaError, ToolCapability, ToolProbe};
pub use casebase::{Case, CaseBase, CaseHit, MIN_CASE_CHARS};
pub use gold::{
    GoldItem, GoldScore, GoldSet, MatchKind, locate_spans as gold_locate_spans,
    merge_scores as gold_merge_scores, normalize as gold_normalize, overlap as gold_overlap,
    parse_gold_rows, parse_gold_set, score as score_gold, score_spanned as score_gold_spanned,
    score_texts as score_gold_texts, span_relation as gold_span_relation,
};
pub use model::{
    Annotation, ApplyMode, Artifacts, CarrierInfo, CarrierKind, ExtractSource, JobMode, LayerKind,
    MarkKind, MarkStyle, PreviewKind, Region, Stitched, TextUnit,
};
pub use docparse::{ParsedDoc, docx_units, parse_docx, parse_pdf, pdf_units};
pub use photo::{
    DEFAULT_GROUP, MARK_PREFIX, PhotoCraft, PhotoCraftConfig, Plan, TypeLayer, collect_type_layers,
    default_candidates_for, not_found_msg, resolve_bin, resolve_tool_bin,
};
pub use pdfcraft::{
    PDF_APP, PDF_CLI, PageParagraphs, PageSize, PdfCraft, PdfCraftConfig, comment_text,
    our_comment_ids, page_box_to_rect, paragraph_units, parse_page_sizes, rect_to_page_box,
    rect_to_quad,
};
pub use wordcraft::{
    WORD_APP, WORD_CLI, Block, WordCraft, WordCraftConfig, apply_commands, block_units, doc_text,
    occurrence_of, our_comment_ids as word_comment_ids, parse_blocks, parse_print_json,
};
pub use report::{
    COUNTED_NO, COUNTED_YES, CarrierAttach, CarrierReplay, CarrierScore, CarrierSource,
    CarrierSpans, REPORT_COLUMNS, ReportFile, ReportMeta, ReportRow, ReproScore, RowOverride,
    RuleVia, apply_selection, attach_gold, attach_gold_by_carrier, attach_gold_spanned, candidates,
    category_label, counted_default, describe_location, missed_rule_evidence, missed_rule_ids,
    render_html, render_json, render_markdown, reproduction, reproduction_by_carrier,
    reproduction_spanned, same_carrier, stitched_spans, to_columns,
};
pub use stitch::{
    DEFAULT_SEPARATOR, crosses_unit, partition_cross_unit, plan_apply, remap, stitch, stitch_units,
    style_for,
};
pub use vision::{
    DEFAULT_VISION_CONFIDENCE, DEFAULT_VISION_SYSTEM, DEFAULT_VISION_USER, VisionBlock,
    bbox_to_region, image_data_url, parse_vision_blocks, vision_mime, vision_units,
};
